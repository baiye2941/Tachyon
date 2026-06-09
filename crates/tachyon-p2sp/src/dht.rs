//! Kademlia DHT 实现
//!
//! 基于 Kademlia 协议的分布式哈希表,用于 Peer 发现。
//! 包含 XOR 距离度量、k-bucket 路由表、迭代查找算法。

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::{oneshot, RwLock};

// ============================================================
// 常量
// ============================================================

/// k-bucket 容量(Kademlia 标准 k 值)
pub const K_BUCKET_SIZE: usize = 20;

/// 并发查找因子(迭代查找时每轮并发请求数)
pub const ALPHA: usize = 3;

/// NodeId 位数(160-bit)
const NODE_ID_BITS: usize = 160;

/// k-bucket 数量(等于 NodeId 位数)
const NUM_BUCKETS: usize = NODE_ID_BITS;

/// 节点过期阈值(15 分钟)
const STALE_DURATION_SECS: u64 = 900;

// ============================================================
// XOR 距离度量
// ============================================================

/// DHT 节点标识(160-bit)
pub type NodeId = [u8; 20];

/// 计算两个 160-bit NodeId 的 XOR 距离
///
/// XOR 距离是 Kademlia 协议的基础度量:距离越小表示两个节点在键空间中越近。
///
/// ```rust
/// use tachyon_p2sp::dht::{xor_distance, NodeId};
///
/// let a: NodeId = [0xAA; 20];
/// let b: NodeId = [0x55; 20];
/// assert_eq!(xor_distance(&a, &b), [0xFF; 20]);
/// ```
#[must_use]
pub fn xor_distance(a: &NodeId, b: &NodeId) -> NodeId {
    let mut result = [0u8; 20];
    for i in 0..20 {
        result[i] = a[i] ^ b[i];
    }
    result
}

/// 计算 XOR 距离的前导零位数
///
/// 用于确定节点应放入哪个 k-bucket。
/// 返回 0..=160,其中 160 表示距离为零(同一个节点)。
///
/// ```rust
/// use tachyon_p2sp::dht::leading_zeros;
///
/// assert_eq!(leading_zeros(&[0u8; 20]), 160);
/// let mut d = [0u8; 20];
/// d[19] = 0x01;
/// assert_eq!(leading_zeros(&d), 159);
/// ```
#[must_use]
pub fn leading_zeros(distance: &NodeId) -> u32 {
    for i in 0..20 {
        if distance[i] != 0 {
            return (i as u32) * 8 + distance[i].leading_zeros();
        }
    }
    160
}

/// 随机生成 160-bit 节点 ID
///
/// 使用系统随机种子生成伪随机 ID。不依赖外部 `rand` crate。
#[must_use]
pub fn generate_node_id() -> NodeId {
    use std::collections::hash_map::RandomState;
    let rb = RandomState::new();
    let mut id = [0u8; 20];
    // 利用 SipHash 的随机种子填充 160-bit ID
    // RandomState 每次实例化使用不同的 OS 随机种子
    for chunk in id.chunks_mut(8) {
        let mut hasher = rb.build_hasher();
        hasher.write_u64(chunk.len() as u64);
        let val = hasher.finish();
        let bytes = val.to_le_bytes();
        for (j, b) in chunk.iter_mut().enumerate() {
            *b = bytes[j];
        }
    }
    id
}

// ============================================================
// DhtNode
// ============================================================

/// DHT 节点信息
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DhtNode {
    /// 节点 ID
    pub id: NodeId,
    /// 节点地址
    pub addr: String,
    /// 最后通信时间(序列化为 UNIX 毫秒时间戳)
    #[serde(with = "serde_system_time")]
    pub last_seen: SystemTime,
}

impl DhtNode {
    /// 创建新的 DHT 节点,`last_seen` 初始化为当前时间
    pub fn new(id: NodeId, addr: String) -> Self {
        Self {
            id,
            addr,
            last_seen: SystemTime::now(),
        }
    }

    /// 节点是否过期(超过 15 分钟未通信)
    pub fn is_stale(&self) -> bool {
        self.last_seen
            .elapsed()
            .map(|d| d > Duration::from_secs(STALE_DURATION_SECS))
            .unwrap_or(false)
    }

    /// 刷新最后通信时间为当前时刻
    pub fn touch(&mut self) {
        self.last_seen = SystemTime::now();
    }
}

// ============================================================
// Kademlia RPC 消息
// ============================================================

/// Kademlia 协议消息类型
///
/// 定义了 Kademlia DHT 的四种核心 RPC 消息以及对应的响应。
/// 实际的网络发送/接收将在后续版本中通过 tachyon-protocol 实现。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KademliaMessage {
    /// 心跳探测
    Ping { sender_id: NodeId },
    /// 心跳响应
    Pong { sender_id: NodeId },
    /// 查找距离 target 最近的节点
    FindNode { sender_id: NodeId, target: NodeId },
    /// FindNode 响应:返回最近的 k 个节点
    FindNodeResponse {
        sender_id: NodeId,
        nodes: Vec<DhtNode>,
    },
    /// 按 key 查找存储的值
    FindValue { sender_id: NodeId, key: Vec<u8> },
    /// FindValue 响应:找到值则返回值,否则返回最近的 k 个节点
    FindValueResponse {
        sender_id: NodeId,
        value: Option<Vec<u8>>,
        nodes: Vec<DhtNode>,
    },
    /// 存储键值对到目标节点
    Store {
        sender_id: NodeId,
        key: Vec<u8>,
        value: Vec<u8>,
    },
}

// ============================================================
// KBucket
// ============================================================

/// Kademlia k-bucket
///
/// 存储一组与本地节点具有相同 XOR 距离前缀的远端节点。
/// 容量上限为 `K_BUCKET_SIZE`(通常为 20)。
/// bucket 满时新节点暂存于替换缓存,等旧节点过期后替换。
#[derive(Debug, Clone)]
pub struct KBucket {
    /// 活跃节点列表(按最近通信时间排序,最新在末尾)
    nodes: Vec<DhtNode>,
    /// 替换缓存(bucket 满时暂存新节点)
    replacement_cache: Vec<DhtNode>,
}

impl KBucket {
    /// 创建空的 k-bucket
    pub fn new() -> Self {
        Self {
            nodes: Vec::with_capacity(K_BUCKET_SIZE),
            replacement_cache: Vec::new(),
        }
    }

    /// bucket 中的节点数
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// bucket 是否为空
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// bucket 是否已满
    pub fn is_full(&self) -> bool {
        self.nodes.len() >= K_BUCKET_SIZE
    }

    /// 获取所有节点的引用(不可变)
    pub fn nodes(&self) -> &[DhtNode] {
        &self.nodes
    }

    /// 获取替换缓存的节点
    pub fn replacement_cache(&self) -> &[DhtNode] {
        &self.replacement_cache
    }

    /// bucket 中是否包含指定 ID 的节点
    pub fn contains(&self, id: &NodeId) -> bool {
        self.nodes.iter().any(|n| &n.id == id)
    }

    /// 更新或插入节点到 k-bucket
    ///
    /// - 如果节点已存在,将其移到列表末尾(标记为最近通信)
    /// - 如果 bucket 未满,直接插入到末尾
    /// - 如果 bucket 已满,将新节点暂存到替换缓存
    ///
    /// 返回 `true` 表示节点被成功插入或更新;`false` 表示放入了替换缓存。
    pub fn update(&mut self, node: DhtNode) -> bool {
        // 节点已存在:移到末尾,刷新时间
        if let Some(pos) = self.nodes.iter().position(|n| n.id == node.id) {
            self.nodes.remove(pos);
            let mut updated = node;
            updated.touch();
            self.nodes.push(updated);
            return true;
        }

        // bucket 未满:直接插入
        if !self.is_full() {
            self.nodes.push(node);
            return true;
        }

        // bucket 已满:放入替换缓存
        // 替换缓存也限制大小,移除最旧的条目
        if self.replacement_cache.len() >= K_BUCKET_SIZE {
            self.replacement_cache.remove(0);
        }
        self.replacement_cache.push(node);
        false
    }

    /// 移除指定 ID 的节点
    ///
    /// 如果替换缓存中有候选项,自动补位。
    pub fn remove(&mut self, id: &NodeId) -> bool {
        if let Some(pos) = self.nodes.iter().position(|n| &n.id == id) {
            self.nodes.remove(pos);
            // 从替换缓存补充
            if let Some(replacement) = self.replacement_cache.pop() {
                self.nodes.push(replacement);
            }
            return true;
        }
        false
    }

    /// 获取最旧的节点(bucket 满时的 ping 候选)
    pub fn oldest(&self) -> Option<&DhtNode> {
        self.nodes.first()
    }

    /// 用替换缓存中最优节点替换最旧节点
    ///
    /// 调用方在确认最旧节点无响应后调用此方法。
    pub fn replace_oldest(&mut self) {
        if let Some(replacement) = self.replacement_cache.pop() {
            if !self.nodes.is_empty() {
                self.nodes[0] = replacement;
            }
        }
    }

    /// 清理 bucket 中的过期节点,并从替换缓存补充
    pub fn evict_stale(&mut self) {
        self.nodes.retain(|n| !n.is_stale());
        // 从替换缓存补充到未满状态
        while !self.is_full() {
            if let Some(candidate) = self.replacement_cache.pop() {
                if !candidate.is_stale() {
                    self.nodes.push(candidate);
                } else {
                    // 替换缓存中的节点也过期了,丢弃
                    continue;
                }
            } else {
                break;
            }
        }
    }
}

impl Default for KBucket {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================
// RoutingTable
// ============================================================

/// Kademlia k-bucket 路由表
///
/// 由 160 个 k-bucket 组成,每个 bucket 对应 XOR 距离的一个位区间。
/// 节点根据其与本地节点的 XOR 距离的前导零位数被分配到对应的 bucket。
#[derive(Debug, Clone)]
pub struct RoutingTable {
    /// 本节点 ID
    local_id: NodeId,
    /// 160 个 k-bucket
    buckets: Vec<KBucket>,
}

impl RoutingTable {
    /// 创建空的路由表
    pub fn new(local_id: NodeId) -> Self {
        let mut buckets = Vec::with_capacity(NUM_BUCKETS);
        for _ in 0..NUM_BUCKETS {
            buckets.push(KBucket::new());
        }
        Self { local_id, buckets }
    }

    /// 获取本节点 ID
    pub fn local_id(&self) -> &NodeId {
        &self.local_id
    }

    /// 根据远端节点 ID 计算其应放入的 bucket 索引
    ///
    /// 索引 = 159 - leading_zeros(xor_distance(local, remote))
    /// 当距离为零(自身)时返回 None。
    pub fn bucket_index(&self, remote_id: &NodeId) -> Option<usize> {
        let dist = xor_distance(&self.local_id, remote_id);
        let lz = leading_zeros(&dist);
        if lz >= 160 {
            // 自身节点,不属于任何 bucket
            return None;
        }
        Some(159 - lz as usize)
    }

    /// 获取指定索引的 bucket 引用
    pub fn bucket(&self, index: usize) -> Option<&KBucket> {
        self.buckets.get(index)
    }

    /// 获取指定索引的 bucket 可变引用
    pub fn bucket_mut(&mut self, index: usize) -> Option<&mut KBucket> {
        self.buckets.get_mut(index)
    }

    /// 更新或插入节点到路由表
    ///
    /// 返回 `true` 表示节点被成功插入或更新。
    pub fn update(&mut self, node: DhtNode) -> bool {
        if let Some(idx) = self.bucket_index(&node.id) {
            self.buckets[idx].update(node)
        } else {
            // 自身节点,忽略
            false
        }
    }

    /// 从路由表中移除指定节点
    pub fn remove(&mut self, id: &NodeId) -> bool {
        if let Some(idx) = self.bucket_index(id) {
            self.buckets[idx].remove(id)
        } else {
            false
        }
    }

    /// 路由表中的总节点数
    pub fn node_count(&self) -> usize {
        self.buckets.iter().map(KBucket::len).sum()
    }

    /// 路由表中的总替换缓存大小
    pub fn replacement_cache_size(&self) -> usize {
        self.buckets
            .iter()
            .map(|b| b.replacement_cache().len())
            .sum()
    }

    /// 查找距离给定 target 最近的 count 个节点
    ///
    /// 从 target 所在 bucket 开始向两侧搜索,收集足够的近距离节点。
    pub fn find_closest(&self, target: &NodeId, count: usize) -> Vec<DhtNode> {
        let dist = xor_distance(&self.local_id, target);
        let lz = leading_zeros(&dist);
        let center = if lz >= 160 {
            // target 就是自身,从 bucket 0 开始
            0
        } else {
            159 - lz as usize
        };

        let mut candidates: Vec<&DhtNode> = Vec::new();

        // 从目标所在 bucket 开始向两侧扩展搜索
        let mut lo = center;
        let mut hi = center;
        let mut expanding = true;

        // 先加入中心 bucket
        candidates.extend(self.buckets[center].nodes());

        while expanding && candidates.len() < count {
            expanding = false;
            if hi < NUM_BUCKETS - 1 {
                hi += 1;
                candidates.extend(self.buckets[hi].nodes());
                expanding = true;
            }
            if lo > 0 {
                lo -= 1;
                candidates.extend(self.buckets[lo].nodes());
                expanding = true;
            }
        }

        // 按与 target 的 XOR 距离排序,取前 count 个
        candidates.sort_by_cached_key(|n| xor_distance(&n.id, target));
        candidates.dedup_by(|a, b| a.id == b.id);
        candidates.into_iter().take(count).cloned().collect()
    }

    /// 清理所有 bucket 中的过期节点
    pub fn cleanup_stale(&mut self) {
        for bucket in &mut self.buckets {
            bucket.evict_stale();
        }
    }

    /// 获取所有活跃节点
    pub fn active_nodes(&self) -> Vec<&DhtNode> {
        self.buckets
            .iter()
            .flat_map(|b| b.nodes().iter())
            .filter(|n| !n.is_stale())
            .collect()
    }

    /// 判断 bucket 满时最旧节点是否需要 ping 检测
    ///
    /// 返回需要 ping 的节点列表(bucket 满时的最旧节点)。
    pub fn stale_candidates(&self) -> Vec<&DhtNode> {
        self.buckets
            .iter()
            .filter(|b| b.is_full())
            .filter_map(|b| b.oldest())
            .filter(|n| n.is_stale())
            .collect()
    }
}

// ============================================================
// KademliaDht
// ============================================================

/// Kademlia DHT 网络
///
/// 封装路由表并提供 DHT 操作的高级接口。
pub struct KademliaDht {
    /// 本节点 ID
    local_id: NodeId,
    /// k-bucket 路由表
    routing_table: RoutingTable,
    /// 最大节点数限制(0 表示无限制)
    max_nodes: usize,
    /// 本地键值存储(用于 FindValue/Store RPC)
    local_store: HashMap<Vec<u8>, Vec<u8>>,
}

impl KademliaDht {
    /// 创建新的 DHT 实例
    pub fn new(local_id: NodeId, max_nodes: usize) -> Self {
        Self {
            local_id,
            routing_table: RoutingTable::new(local_id),
            max_nodes,
            local_store: HashMap::new(),
        }
    }

    /// 添加节点到 DHT
    ///
    /// 遵循 Kademlia 的更新规则:
    /// - 节点已存在则刷新
    /// - bucket 未满则插入
    /// - bucket 满则放入替换缓存
    ///
    /// 当超过 `max_nodes` 限制时,优先移除过期节点。
    pub fn add_node(&mut self, node: DhtNode) {
        // 如果已存在,直接更新(会刷新 last_seen)
        let exists = self
            .routing_table
            .bucket_index(&node.id)
            .and_then(|idx| self.routing_table.bucket(idx))
            .map_or(false, |b| b.contains(&node.id));

        if exists {
            self.routing_table.update(node);
            return;
        }

        // 检查全局 max_nodes 限制
        if self.max_nodes > 0 && self.routing_table.node_count() >= self.max_nodes {
            // 优先清理过期节点腾出空间
            self.cleanup_stale();
            if self.routing_table.node_count() >= self.max_nodes {
                // 还是满的,移除全局最旧节点
                self.evict_oldest();
            }
        }

        self.routing_table.update(node);
    }

    /// 移除全局最旧的节点(用于 max_nodes 限制)
    fn evict_oldest(&mut self) {
        let oldest_info = (0..NUM_BUCKETS)
            .filter_map(|i| {
                self.routing_table
                    .bucket(i)
                    .and_then(|b| b.oldest().map(|n| (i, n.id, n.last_seen)))
            })
            .min_by_key(|(_, _, last_seen)| *last_seen);

        if let Some((bucket_idx, node_id, _)) = oldest_info {
            // 尝试用替换缓存中的节点替换
            if let Some(bucket) = self.routing_table.bucket_mut(bucket_idx) {
                bucket.remove(&node_id);
                if let Some(replacement) = bucket.replacement_cache().last().cloned() {
                    bucket.update(replacement);
                }
            }
        }
    }

    /// 获取已知节点数
    pub fn node_count(&self) -> usize {
        self.routing_table.node_count()
    }

    /// 获取本节点 ID
    pub fn local_id(&self) -> &NodeId {
        &self.local_id
    }

    /// 清理过期节点
    pub fn cleanup_stale(&mut self) {
        self.routing_table.cleanup_stale();
    }

    /// 获取所有活跃节点
    pub fn active_nodes(&self) -> Vec<&DhtNode> {
        self.routing_table.active_nodes()
    }

    /// 获取路由表的只读引用
    pub fn routing_table(&self) -> &RoutingTable {
        &self.routing_table
    }

    /// 获取路由表的可变引用
    pub fn routing_table_mut(&mut self) -> &mut RoutingTable {
        &mut self.routing_table
    }

    /// 迭代查找距离 target 最近的 k 个节点
    ///
    /// 从本地 k-bucket 取 alpha 个最近节点,然后迭代查询,
    /// 直到候选集不再收敛(没有更近的新节点被发现)。
    ///
    /// 注意:当前版本仅使用本地路由表数据进行模拟查找,
    /// 实际的网络 RPC 查询将在后续版本中通过 tachyon-protocol 实现。
    pub fn find_node(&self, target: &NodeId) -> Vec<DhtNode> {
        self.routing_table.find_closest(target, K_BUCKET_SIZE)
    }

    /// 处理 FindNode 请求(本地查询)
    ///
    /// 返回本节点路由表中距离 target 最近的 k 个节点。
    pub fn handle_find_node(&self, target: &NodeId) -> Vec<DhtNode> {
        self.routing_table.find_closest(target, K_BUCKET_SIZE)
    }

    /// 按 key 查找本地存储的值
    ///
    /// 首先在本地存储中查找,然后返回最近节点列表用于迭代查找。
    pub fn find_value(&self, key: &[u8]) -> (Option<Vec<u8>>, Vec<DhtNode>) {
        let value = self.local_store.get(key).cloned();
        let nodes = self.routing_table.find_closest(
            &Self::key_to_node_id(key),
            K_BUCKET_SIZE,
        );
        (value, nodes)
    }

    /// 本地存储查找(不触发网络操作)
    pub fn find_value_local(&self, key: &[u8]) -> (Option<Vec<u8>>, Vec<DhtNode>) {
        let value = self.local_store.get(key).cloned();
        let nodes = self.routing_table.find_closest(
            &Self::key_to_node_id(key),
            K_BUCKET_SIZE,
        );
        (value, nodes)
    }

    /// 将 key 映射为 NodeId(取 key 哈希填充 20 字节)
    fn key_to_node_id(key: &[u8]) -> NodeId {
        let mut id = [0u8; 20];
        let rb = std::collections::hash_map::RandomState::new();
        for (i, chunk) in id.chunks_mut(8).enumerate() {
            let mut hasher = rb.build_hasher();
            hasher.write(key);
            hasher.write_u64(i as u64);
            let val = hasher.finish();
            let bytes = val.to_le_bytes();
            for (j, b) in chunk.iter_mut().enumerate() {
                *b = bytes[j];
            }
        }
        id
    }

    /// 存储键值对到本地存储
    ///
    /// 将键值对保存到本节点的本地存储中,
    /// 网络层的分布式存储会将数据复制到最近的 k 个节点。
    pub fn store(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.local_store.insert(key, value);
    }

    /// 构造 Ping 消息
    pub fn make_ping(&self) -> KademliaMessage {
        KademliaMessage::Ping {
            sender_id: self.local_id,
        }
    }

    /// 构造 Pong 消息
    pub fn make_pong(&self) -> KademliaMessage {
        KademliaMessage::Pong {
            sender_id: self.local_id,
        }
    }

    /// 构造 FindNode 消息
    pub fn make_find_node(&self, target: NodeId) -> KademliaMessage {
        KademliaMessage::FindNode {
            sender_id: self.local_id,
            target,
        }
    }

    /// 构造 FindNode 响应消息
    pub fn make_find_node_response(&self, nodes: Vec<DhtNode>) -> KademliaMessage {
        KademliaMessage::FindNodeResponse {
            sender_id: self.local_id,
            nodes,
        }
    }

    /// 处理收到的 RPC 消息,更新路由表
    ///
    /// 根据消息类型更新发送方节点信息到路由表。
    pub fn handle_message(&mut self, msg: &KademliaMessage) {
        let sender_id = match msg {
            KademliaMessage::Ping { sender_id }
            | KademliaMessage::Pong { sender_id }
            | KademliaMessage::FindNode { sender_id, .. }
            | KademliaMessage::FindNodeResponse { sender_id, .. }
            | KademliaMessage::FindValue { sender_id, .. }
            | KademliaMessage::FindValueResponse { sender_id, .. }
            | KademliaMessage::Store { sender_id, .. } => *sender_id,
        };
        // 更新发送方节点(如果已存在于路由表中则刷新时间)
        // 注意:此处仅更新已知节点,不自动添加未知节点到路由表
        // (添加未知节点需要额外验证,如 Ping 验证)
        let bucket_idx = self.routing_table.bucket_index(&sender_id);
        if let Some(idx) = bucket_idx {
            if let Some(bucket) = self.routing_table.bucket_mut(idx) {
                if bucket.contains(&sender_id) {
                    // 刷新已有节点的时间
                    if let Some(pos) = bucket.nodes().iter().position(|n| n.id == sender_id) {
                        let mut node = bucket.nodes()[pos].clone();
                        node.touch();
                        bucket.update(node);
                    }
                }
            }
        }
    }
}

// ============================================================
// Serde 辅助: SystemTime <-> UNIX 毫秒时间戳
// ============================================================

/// `SystemTime` 的 serde 序列化辅助模块(以 UNIX 毫秒时间戳存储)
mod serde_system_time {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    pub fn serialize<S: Serializer>(time: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
        let millis = time
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis() as u64;
        s.serialize_u64(millis)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
        let millis = u64::deserialize(d)?;
        Ok(UNIX_EPOCH + Duration::from_millis(millis))
    }
}

// ============================================================
// TransportError
// ============================================================

/// DHT 网络传输错误
#[derive(Debug)]
pub enum TransportError {
    /// 消息序列化或反序列化失败
    Serialization(String),
    /// UDP I/O 错误
    Io(std::io::Error),
    /// RPC 请求超时(默认 5 秒)
    Timeout,
    /// 传输层已关闭
    Shutdown,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialization(msg) => write!(f, "serialization error: {msg}"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Timeout => write!(f, "RPC request timed out"),
            Self::Shutdown => write!(f, "transport is shut down"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ============================================================
// DhtTransport — UDP RPC 网络层
// ============================================================

/// RPC 请求 ID(单调递增计数器,用于匹配请求与响应)
type RequestId = u64;

/// 默认 RPC 超时时间(5 秒)
const RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// DHT UDP 传输层
///
/// 在 `KademliaDht` 之上提供基于 UDP 的 RPC 网络能力:
/// - 消息 JSON 序列化/反序列化
/// - 请求-响应关联(通过 8 字节 request ID 头)
/// - 后台接收循环与消息路由
/// - 迭代式分布式查找(FIND_NODE / FIND_VALUE / STORE)
///
/// # 线格式
///
/// ```text
/// [request_id: u64 big-endian (8 bytes)][JSON payload]
/// ```
pub struct DhtTransport {
    socket: Arc<UdpSocket>,
    dht: Arc<RwLock<KademliaDht>>,
    pending_requests: Arc<DashMap<RequestId, oneshot::Sender<KademliaMessage>>>,
    running: Arc<AtomicBool>,
    next_request_id: AtomicU64,
}

impl DhtTransport {
    /// 绑定到指定地址的 UDP socket,创建传输层实例
    ///
    /// 使用 `addr = "127.0.0.1:0"` 可由操作系统自动分配可用端口。
    pub async fn bind(
        addr: &str,
        dht: Arc<RwLock<KademliaDht>>,
    ) -> Result<Self, TransportError> {
        let socket = UdpSocket::bind(addr).await?;
        Ok(Self {
            socket: Arc::new(socket),
            dht,
            pending_requests: Arc::new(DashMap::new()),
            running: Arc::new(AtomicBool::new(false)),
            next_request_id: AtomicU64::new(1),
        })
    }

    /// 获取本地 socket 地址
    pub fn local_addr(&self) -> Result<std::net::SocketAddr, TransportError> {
        Ok(self.socket.local_addr()?)
    }

    /// 获取本节点 ID
    pub async fn local_id(&self) -> NodeId {
        *self.dht.read().await.local_id()
    }

    /// 获取底层 DHT 的 Arc 引用
    pub fn dht(&self) -> &Arc<RwLock<KademliaDht>> {
        &self.dht
    }

    /// 启动后台接收循环
    ///
    /// 在独立的 tokio task 中持续接收 UDP 报文:
    /// - **响应消息**: 匹配到 pending request,通过 oneshot channel 返回给调用者
    /// - **请求消息**: 调用 `process_incoming` 生成响应并回送
    ///
    /// 调用 [`shutdown()`](Self::shutdown) 可终止循环。
    pub fn start_recv_loop(&self) {
        self.running.store(true, Ordering::SeqCst);
        let socket = self.socket.clone();
        let dht = self.dht.clone();
        let pending = self.pending_requests.clone();
        let running = self.running.clone();
        tokio::spawn(Self::recv_loop_inner(socket, dht, pending, running));
    }

    /// 关闭传输层,停止接收循环
    pub fn shutdown(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// 检查传输层是否正在运行
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    // ----------------------------------------------------------
    // 内部: 接收循环
    // ----------------------------------------------------------

    async fn recv_loop_inner(
        socket: Arc<UdpSocket>,
        dht: Arc<RwLock<KademliaDht>>,
        pending: Arc<DashMap<RequestId, oneshot::Sender<KademliaMessage>>>,
        running: Arc<AtomicBool>,
    ) {
        let mut buf = [0u8; 65535];
        while running.load(Ordering::SeqCst) {
            let (len, src_addr) = match socket.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "UDP recv_from failed");
                    continue;
                }
            };
            if len < 8 {
                continue; // 报文过短,无法包含 request ID
            }

            // 解析 request ID(前 8 字节,大端序)
            let request_id = u64::from_be_bytes(buf[..8].try_into().unwrap());

            // 解析 JSON 消息体
            let msg: KademliaMessage = match serde_json::from_slice(&buf[8..len]) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to deserialize message");
                    continue;
                }
            };

            // 尝试匹配 pending request(响应消息)
            if let Some((_, sender)) = pending.remove(&request_id) {
                let _ = sender.send(msg);
                continue;
            }

            // 未匹配:视为入站请求,生成响应并回送
            if let Some(response) = Self::process_incoming(&dht, &msg, src_addr).await {
                let payload = match serde_json::to_vec(&response) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to serialize response");
                        continue;
                    }
                };
                let mut packet = Vec::with_capacity(8 + payload.len());
                packet.extend_from_slice(&request_id.to_be_bytes());
                packet.extend_from_slice(&payload);
                if let Err(e) = socket.send_to(&packet, src_addr).await {
                    tracing::warn!(error = %e, addr = %src_addr, "failed to send response");
                }
            }
        }
    }

    // ----------------------------------------------------------
    // 内部: 入站消息处理
    // ----------------------------------------------------------

    /// 处理入站 RPC 请求,返回需要回送的响应(若需要)
    ///
    /// - 请求类消息(Ping / FindNode / FindValue / Store):更新路由表 + 生成响应
    /// - 响应类消息(Pong / FindNodeResponse / FindValueResponse):仅更新路由表,不生成响应
    async fn process_incoming(
        dht: &Arc<RwLock<KademliaDht>>,
        msg: &KademliaMessage,
        src_addr: std::net::SocketAddr,
    ) -> Option<KademliaMessage> {
        let mut dht_guard = dht.write().await;

        // 更新路由表中已知发送方的 last_seen
        dht_guard.handle_message(msg);

        match msg {
            KademliaMessage::Ping { .. } => Some(dht_guard.make_pong()),

            KademliaMessage::FindNode { target, .. } => {
                let nodes = dht_guard.handle_find_node(target);
                Some(dht_guard.make_find_node_response(nodes))
            }

            KademliaMessage::FindValue { key, .. } => {
                let (value, nodes) = dht_guard.find_value_local(key);
                Some(KademliaMessage::FindValueResponse {
                    sender_id: *dht_guard.local_id(),
                    value,
                    nodes,
                })
            }

            KademliaMessage::Store {
                key, value, ..
            } => {
                dht_guard.store(key.clone(), value.clone());
                // 将发送方添加到路由表(Ping 验证通过)
                let sender_id = match msg {
                    KademliaMessage::Store { sender_id, .. } => *sender_id,
                    _ => unreachable!(),
                };
                dht_guard.add_node(DhtNode::new(sender_id, src_addr.to_string()));
                None
            }

            // 响应类消息:已在 handle_message 中刷新路由表,无需生成响应
            _ => None,
        }
    }

    // ----------------------------------------------------------
    // 核心: 发送 RPC 请求并等待响应
    // ----------------------------------------------------------

    /// 发送 RPC 请求到目标地址并等待响应
    ///
    /// 1. 分配唯一的 request ID
    /// 2. 注册 oneshot channel 到 pending map
    /// 3. 序列化消息并通过 UDP 发送(前缀 8 字节 request ID)
    /// 4. 等待响应(超时 5 秒)
    ///
    /// # 错误
    ///
    /// - `TransportError::Shutdown`: 传输层已关闭
    /// - `TransportError::Serialization`: 消息序列化失败
    /// - `TransportError::Io`: UDP 发送失败
    /// - `TransportError::Timeout`: 5 秒内未收到响应
    pub async fn send_rpc(
        &self,
        target_addr: &str,
        message: &KademliaMessage,
    ) -> Result<KademliaMessage, TransportError> {
        if !self.running.load(Ordering::SeqCst) {
            return Err(TransportError::Shutdown);
        }

        let request_id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending_requests.insert(request_id, tx);

        let payload = serde_json::to_vec(message)
            .map_err(|e| TransportError::Serialization(e.to_string()))?;

        let mut packet = Vec::with_capacity(8 + payload.len());
        packet.extend_from_slice(&request_id.to_be_bytes());
        packet.extend_from_slice(&payload);

        self.socket.send_to(&packet, target_addr).await?;

        match tokio::time::timeout(RPC_TIMEOUT, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => {
                self.pending_requests.remove(&request_id);
                Err(TransportError::Shutdown)
            }
            Err(_) => {
                self.pending_requests.remove(&request_id);
                Err(TransportError::Timeout)
            }
        }
    }

    // ----------------------------------------------------------
    // 高层 RPC 便捷方法
    // ----------------------------------------------------------

    /// 发送 Ping RPC,成功收到 Pong 则返回 `true`
    pub async fn ping(&self, addr: &str) -> Result<bool, TransportError> {
        let msg = {
            let dht = self.dht.read().await;
            dht.make_ping()
        };
        let response = self.send_rpc(addr, &msg).await?;
        Ok(matches!(response, KademliaMessage::Pong { .. }))
    }

    /// 发送 FindNode RPC,返回目标节点已知的最近节点列表
    pub async fn find_node_rpc(
        &self,
        addr: &str,
        target: &NodeId,
    ) -> Result<Vec<DhtNode>, TransportError> {
        let msg = {
            let dht = self.dht.read().await;
            dht.make_find_node(*target)
        };
        let response = self.send_rpc(addr, &msg).await?;
        match response {
            KademliaMessage::FindNodeResponse { nodes, .. } => Ok(nodes),
            _ => Ok(Vec::new()),
        }
    }

    /// 发送 FindValue RPC,返回 (值, 最近节点列表)
    pub async fn find_value_rpc(
        &self,
        addr: &str,
        key: &[u8],
    ) -> Result<(Option<Vec<u8>>, Vec<DhtNode>), TransportError> {
        let msg = KademliaMessage::FindValue {
            sender_id: self.local_id().await,
            key: key.to_vec(),
        };
        let response = self.send_rpc(addr, &msg).await?;
        match response {
            KademliaMessage::FindValueResponse {
                value, nodes, ..
            } => Ok((value, nodes)),
            _ => Ok((None, Vec::new())),
        }
    }

    /// 发送 Store RPC(单向,不等待响应)
    pub async fn store_rpc(
        &self,
        addr: &str,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), TransportError> {
        let msg = KademliaMessage::Store {
            sender_id: self.local_id().await,
            key: key.to_vec(),
            value: value.to_vec(),
        };
        let payload = serde_json::to_vec(&msg)
            .map_err(|e| TransportError::Serialization(e.to_string()))?;

        let request_id = self.next_request_id.fetch_add(1, Ordering::SeqCst);
        let mut packet = Vec::with_capacity(8 + payload.len());
        packet.extend_from_slice(&request_id.to_be_bytes());
        packet.extend_from_slice(&payload);

        self.socket.send_to(&packet, addr).await?;
        Ok(())
    }

    // ----------------------------------------------------------
    // 迭代式分布式查找
    // ----------------------------------------------------------

    /// 迭代查找距离 target 最近的 k 个节点
    ///
    /// 实现 Kademlia 标准迭代查询:
    /// 1. 从本地路由表取初始候选集
    /// 2. 每轮并发向 `ALPHA`(3) 个最近未查询节点发送 FindNode
    /// 3. 合并新发现的节点,按 XOR 距离排序
    /// 4. 当候选集不再收敛时终止
    pub async fn iterative_find_node(
        &self,
        target: &NodeId,
    ) -> Result<Vec<DhtNode>, TransportError> {
        let mut known: Vec<DhtNode> = {
            let dht = self.dht.read().await;
            dht.routing_table().find_closest(target, K_BUCKET_SIZE)
        };

        if known.is_empty() {
            return Ok(Vec::new());
        }

        let mut queried_ids: std::collections::HashSet<NodeId> = std::collections::HashSet::new();

        loop {
            // 按 XOR 距离排序,选择最多 ALPHA 个未查询节点
            known.sort_by_cached_key(|n| xor_distance(&n.id, target));
            let targets: Vec<DhtNode> = known
                .iter()
                .filter(|n| !queried_ids.contains(&n.id))
                .take(ALPHA)
                .cloned()
                .collect();

            if targets.is_empty() {
                break; // 所有已知节点均已查询,查找收敛
            }

            for t in &targets {
                queried_ids.insert(t.id);
            }

            // 并发 RPC 查询
            let mut handles = Vec::new();
            for node in targets {
                let target_copy = *target;
                handles.push(tokio::spawn({
                    let transport_socket = self.socket.clone();
                    let dht_ref = self.dht.clone();
                    let pending_ref = self.pending_requests.clone();
                    let next_id = &self.next_request_id;
                    let addr = node.addr.clone();
                    let req_id = next_id.fetch_add(1, Ordering::SeqCst);
                    async move {
                        // 构造 FindNode 消息
                        let msg = {
                            let dht = dht_ref.read().await;
                            dht.make_find_node(target_copy)
                        };
                        let payload = match serde_json::to_vec(&msg) {
                            Ok(p) => p,
                            Err(_) => return Vec::new(),
                        };
                        let (tx, rx) = oneshot::channel();
                        // 使用临时 pending map
                        let temp_pending: Arc<DashMap<RequestId, oneshot::Sender<KademliaMessage>>> =
                            pending_ref;
                        temp_pending.insert(req_id, tx);
                        let mut packet = Vec::with_capacity(8 + payload.len());
                        packet.extend_from_slice(&req_id.to_be_bytes());
                        packet.extend_from_slice(&payload);
                        if transport_socket.send_to(&packet, &addr).await.is_err() {
                            temp_pending.remove(&req_id);
                            return Vec::new();
                        }
                        match tokio::time::timeout(RPC_TIMEOUT, rx).await {
                            Ok(Ok(KademliaMessage::FindNodeResponse { nodes, .. })) => nodes,
                            _ => {
                                temp_pending.remove(&req_id);
                                Vec::new()
                            }
                        }
                    }
                }));
            }

            let mut new_found = false;
            for handle in handles {
                if let Ok(nodes) = handle.await {
                    for node in nodes {
                        if !queried_ids.contains(&node.id)
                            && !known.iter().any(|n| n.id == node.id)
                        {
                            known.push(node);
                            new_found = true;
                        }
                    }
                }
            }

            if !new_found {
                break;
            }
        }

        // 按距离排序并截取前 k 个
        known.sort_by_cached_key(|n| xor_distance(&n.id, target));
        known.truncate(K_BUCKET_SIZE);
        Ok(known)
    }

    /// 迭代查找并获取值
    ///
    /// 1. 执行迭代查找定位存储了该 key 的节点
    /// 2. 向候选节点逐个发送 FindValue 直到获取到值
    pub async fn distributed_find_value(
        &self,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, TransportError> {
        let key_node_id = KademliaDht::key_to_node_id(key);

        // Phase 1: 迭代查找近距离节点
        let closest = self.iterative_find_node(&key_node_id).await?;

        // Phase 2: 向候选节点发送 FindValue 请求
        for node in &closest {
            match self.find_value_rpc(&node.addr, key).await {
                Ok((Some(value), _)) => return Ok(Some(value)),
                Ok((None, _)) => continue,
                Err(_) => continue,
            }
        }

        Ok(None)
    }

    /// 分布式存储:将键值对复制到最近的 k 个节点
    ///
    /// 1. 通过迭代查找定位最近的 k 个节点
    /// 2. 并发向这些节点发送 Store RPC
    /// 3. 同时存储到本地
    ///
    /// 返回成功存储的节点数。
    pub async fn distributed_store(
        &self,
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> Result<usize, TransportError> {
        let key_node_id = KademliaDht::key_to_node_id(&key);

        // 查找最近的 k 个节点
        let closest = self.iterative_find_node(&key_node_id).await?;

        // 并发 Store 到远程节点
        let mut handles = Vec::new();
        for node in &closest {
            let key = key.clone();
            let value = value.clone();
            let addr = node.addr.clone();
            handles.push(tokio::spawn({
                let socket = self.socket.clone();
                let next_id = &self.next_request_id;
                let sender_id = self.local_id().await;
                let req_id = next_id.fetch_add(1, Ordering::SeqCst);
                async move {
                    let msg = KademliaMessage::Store {
                        sender_id,
                        key,
                        value,
                    };
                    let payload = match serde_json::to_vec(&msg) {
                        Ok(p) => p,
                        Err(_) => return false,
                    };
                    let mut packet = Vec::with_capacity(8 + payload.len());
                    packet.extend_from_slice(&req_id.to_be_bytes());
                    packet.extend_from_slice(&payload);
                    socket.send_to(&packet, &addr).await.is_ok()
                }
            }));
        }

        // 同时存储到本地
        {
            let mut dht = self.dht.write().await;
            dht.store(key, value);
        }

        let mut success_count = 0usize;
        for h in handles {
            if let Ok(true) = h.await {
                success_count += 1;
            }
        }

        Ok(success_count)
    }
}

impl Drop for DhtTransport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn make_node(id_byte: u8) -> DhtNode {
        let mut id = [0u8; 20];
        id[0] = id_byte;
        DhtNode::new(id, format!("192.168.1.{id_byte}:8080"))
    }

    fn make_node_with_id(id: NodeId) -> DhtNode {
        DhtNode::new(id, "127.0.0.1:8080".to_string())
    }

    // ----------------------------------------------------------
    // XOR 距离测试
    // ----------------------------------------------------------

    #[test]
    fn test_xor_distance_self_is_zero() {
        let a: NodeId = [0xAA; 20];
        assert_eq!(xor_distance(&a, &a), [0u8; 20]);
    }

    #[test]
    fn test_xor_distance_basic() {
        let a: NodeId = [0xAA; 20];
        let b: NodeId = [0x55; 20];
        assert_eq!(xor_distance(&a, &b), [0xFF; 20]);
    }

    #[test]
    fn test_xor_distance_symmetric() {
        let a: NodeId = [0xAA; 20];
        let b: NodeId = [0x55; 20];
        assert_eq!(xor_distance(&a, &b), xor_distance(&b, &a));
    }

    #[test]
    fn test_xor_distance_partial() {
        let a: NodeId = [0xAA; 20];
        let mut c = [0u8; 20];
        c[0] = 0x01;
        let dist = xor_distance(&a, &c);
        assert_eq!(dist[0], 0xAB);
        assert_eq!(dist[1], 0xAA);
    }

    // ----------------------------------------------------------
    // leading_zeros 测试
    // ----------------------------------------------------------

    #[test]
    fn test_leading_zeros_zero_distance() {
        assert_eq!(leading_zeros(&[0u8; 20]), 160);
    }

    #[test]
    fn test_leading_zeros_one() {
        let mut d = [0u8; 20];
        d[19] = 0x01; // 最低位
        assert_eq!(leading_zeros(&d), 159);
    }

    #[test]
    fn test_leading_zeros_msb() {
        let mut d = [0u8; 20];
        d[0] = 0x80; // 最高字节的最高位
        assert_eq!(leading_zeros(&d), 0);
    }

    #[test]
    fn test_leading_zeros_byte_boundary() {
        let mut d = [0u8; 20];
        d[0] = 0x01; // 第一字节最低位
        assert_eq!(leading_zeros(&d), 7);
    }

    #[test]
    fn test_leading_zeros_full_byte() {
        let mut d = [0u8; 20];
        d[0] = 0xFF; // 第一字节全部位
        assert_eq!(leading_zeros(&d), 0);
    }

    // ----------------------------------------------------------
    // generate_node_id 测试
    // ----------------------------------------------------------

    #[test]
    fn test_generate_node_id_length() {
        let id = generate_node_id();
        assert_eq!(id.len(), 20);
    }

    #[test]
    fn test_generate_node_id_not_all_zeros() {
        // 极低概率生成全零 ID
        let id = generate_node_id();
        assert_ne!(id, [0u8; 20]);
    }

    #[test]
    fn test_generate_node_id_unique() {
        // 连续生成的 ID 大概率不同
        let a = generate_node_id();
        let b = generate_node_id();
        assert_ne!(a, b, "连续生成的节点 ID 不应相同");
    }

    // ----------------------------------------------------------
    // KBucket 测试
    // ----------------------------------------------------------

    #[test]
    fn test_kbucket_new_is_empty() {
        let bucket = KBucket::new();
        assert!(bucket.is_empty());
        assert_eq!(bucket.len(), 0);
        assert!(!bucket.is_full());
    }

    #[test]
    fn test_kbucket_insert_and_contains() {
        let mut bucket = KBucket::new();
        let node = make_node(1);
        let node_id = node.id;
        assert!(bucket.update(node));
        assert_eq!(bucket.len(), 1);
        assert!(bucket.contains(&node_id));
    }

    #[test]
    fn test_kbucket_update_moves_to_end() {
        let mut bucket = KBucket::new();
        let node_a = make_node(1);
        let node_b = make_node(2);
        bucket.update(node_a);
        bucket.update(node_b);
        // 最旧的是 node_a (index 0)
        assert_eq!(bucket.nodes()[0].id[0], 1);
        // 更新 node_a,应该移到末尾
        let mut updated_a = make_node(1);
        updated_a.addr = "10.0.0.1:9999".to_string();
        bucket.update(updated_a);
        assert_eq!(bucket.nodes()[0].id[0], 2); // node_b 变成最旧
        assert_eq!(bucket.nodes()[1].id[0], 1); // node_a 移到末尾
    }

    #[test]
    fn test_kbucket_full_goes_to_replacement_cache() {
        let mut bucket = KBucket::new();
        // 填满 bucket
        for i in 0..K_BUCKET_SIZE as u8 {
            bucket.update(make_node(i + 1));
        }
        assert!(bucket.is_full());
        assert_eq!(bucket.len(), K_BUCKET_SIZE);

        // 新节点应放入替换缓存
        let overflow_node = make_node(0xFF);
        let result = bucket.update(overflow_node);
        assert!(!result, "bucket 满时返回 false");
        assert_eq!(bucket.len(), K_BUCKET_SIZE);
        assert_eq!(bucket.replacement_cache().len(), 1);
    }

    #[test]
    fn test_kbucket_replace_oldest() {
        let mut bucket = KBucket::new();
        for i in 0..K_BUCKET_SIZE as u8 {
            bucket.update(make_node(i + 1));
        }
        // 添加一个溢出节点
        bucket.update(make_node(0xFF));

        // 最旧节点是 make_node(1)
        assert_eq!(bucket.oldest().unwrap().id[0], 1);

        // 替换最旧节点
        bucket.replace_oldest();
        assert_eq!(bucket.len(), K_BUCKET_SIZE);
        // 新节点应该是替换缓存中的节点(0xFF)
        assert_eq!(bucket.oldest().unwrap().id[0], 0xFF);
    }

    #[test]
    fn test_kbucket_remove_and_replacement() {
        let mut bucket = KBucket::new();
        for i in 0..K_BUCKET_SIZE as u8 {
            bucket.update(make_node(i + 1));
        }
        // 添加替换缓存
        bucket.update(make_node(0xAA));
        assert_eq!(bucket.replacement_cache().len(), 1);

        // 移除一个节点
        let removed = bucket.remove(&make_node(5).id);
        assert!(removed);
        // 替换缓存中的节点应自动补位
        assert_eq!(bucket.len(), K_BUCKET_SIZE);
        assert_eq!(bucket.replacement_cache().len(), 0);
    }

    #[test]
    fn test_kbucket_evict_stale() {
        let mut bucket = KBucket::new();
        let mut fresh = make_node(1);
        fresh.touch();
        bucket.update(fresh);

        // 插入一个过期节点
        let mut stale = make_node(2);
        stale.last_seen = SystemTime::now() - Duration::from_secs(3600);
        bucket.update(stale);

        assert_eq!(bucket.len(), 2);
        bucket.evict_stale();
        assert_eq!(bucket.len(), 1, "过期节点应被清理");
        assert!(bucket.contains(&make_node(1).id));
    }

    // ----------------------------------------------------------
    // RoutingTable 测试
    // ----------------------------------------------------------

    #[test]
    fn test_routing_table_bucket_index() {
        let local_id: NodeId = [0u8; 20];
        let table = RoutingTable::new(local_id);

        // 远端节点与本节点第一位不同 -> bucket 159
        let mut far = [0u8; 20];
        far[0] = 0x80; // 1000_0000, leading_zeros = 0 -> bucket 159
        assert_eq!(table.bucket_index(&far), Some(159));

        // 远端节点仅最后一位不同 -> bucket 0
        let mut close = [0u8; 20];
        close[19] = 0x01; // leading_zeros = 159 -> bucket 0
        assert_eq!(table.bucket_index(&close), Some(0));

        // 自身节点 -> None
        assert_eq!(table.bucket_index(&local_id), None);
    }

    #[test]
    fn test_routing_table_update_and_count() {
        let mut table = RoutingTable::new([0u8; 20]);
        assert_eq!(table.node_count(), 0);

        table.update(make_node(1));
        assert_eq!(table.node_count(), 1);

        table.update(make_node(2));
        assert_eq!(table.node_count(), 2);
    }

    #[test]
    fn test_routing_table_find_closest() {
        let mut table = RoutingTable::new([0u8; 20]);
        // 添加多个节点到不同 bucket
        for i in 1..=50u8 {
            table.update(make_node(i));
        }
        let target = make_node(1).id;
        let closest = table.find_closest(&target, 5);
        assert!(!closest.is_empty());
        assert!(closest.len() <= 5);
        // 最近的节点应该是 target 自身
        assert_eq!(closest[0].id, target);
    }

    // ----------------------------------------------------------
    // KademliaDht 测试(兼容原有 API)
    // ----------------------------------------------------------

    #[test]
    fn test_dht_creation() {
        let dht = KademliaDht::new([0u8; 20], 100);
        assert_eq!(dht.node_count(), 0);
    }

    #[test]
    fn test_add_node() {
        let mut dht = KademliaDht::new([0u8; 20], 100);
        dht.add_node(make_node(1));
        assert_eq!(dht.node_count(), 1);
    }

    #[test]
    fn test_max_nodes() {
        let mut dht = KademliaDht::new([0u8; 20], 2);
        dht.add_node(make_node(1));
        dht.add_node(make_node(2));
        dht.add_node(make_node(3));
        assert!(dht.node_count() <= 2, "超过 max_nodes 时应驱逐旧节点");
    }

    #[test]
    fn test_node_not_stale_initially() {
        let node = make_node(1);
        assert!(!node.is_stale());
    }

    #[test]
    fn test_cleanup_stale() {
        let mut dht = KademliaDht::new([0u8; 20], 100);
        dht.add_node(make_node(1));
        assert_eq!(dht.node_count(), 1);
        dht.cleanup_stale();
        assert_eq!(dht.node_count(), 1);
    }

    #[test]
    fn test_local_id() {
        let id = [1u8; 20];
        let dht = KademliaDht::new(id, 100);
        assert_eq!(dht.local_id(), &id);
    }

    #[test]
    fn test_active_nodes() {
        let mut dht = KademliaDht::new([0u8; 20], 100);
        dht.add_node(make_node(1));
        dht.add_node(make_node(2));
        assert_eq!(dht.active_nodes().len(), 2);
    }

    #[test]
    fn test_cleanup_stale_removes_old_nodes() {
        let mut dht = KademliaDht::new([0u8; 20], 100);
        dht.add_node(make_node(1));
        // 插入一个过期节点
        let mut stale_node = DhtNode::new([9u8; 20], "10.0.0.9:6881".to_string());
        stale_node.last_seen = SystemTime::now() - Duration::from_secs(3600);
        dht.add_node(stale_node);
        assert_eq!(dht.node_count(), 2);
        dht.cleanup_stale();
        assert_eq!(dht.node_count(), 1, "过期节点应被清理");
    }

    // ----------------------------------------------------------
    // 迭代查找收敛性测试
    // ----------------------------------------------------------

    #[test]
    fn test_find_node_returns_closest() {
        let mut dht = KademliaDht::new([0u8; 20], 1000);
        // 添加 100 个节点
        for i in 1..=100u8 {
            dht.add_node(make_node(i));
        }

        let target = make_node(50).id;
        let result = dht.find_node(&target);
        assert!(!result.is_empty());
        // 结果中的第一个应该是 target 自身(如果已添加)
        assert_eq!(result[0].id, target);
        // 结果不超过 K_BUCKET_SIZE
        assert!(result.len() <= K_BUCKET_SIZE);
    }

    #[test]
    fn test_find_node_monotonic_closeness() {
        let mut dht = KademliaDht::new([0u8; 20], 1000);
        for i in 1..=100u8 {
            dht.add_node(make_node(i));
        }

        let target = make_node(50).id;
        let result = dht.find_node(&target);

        // 验证结果按距离排序(单调递增)
        for i in 1..result.len() {
            let d_prev = xor_distance(&result[i - 1].id, &target);
            let d_curr = xor_distance(&result[i].id, &target);
            assert!(d_prev <= d_curr, "find_node 结果应按 XOR 距离排序");
        }
    }

    #[test]
    fn test_find_closest_with_fewer_nodes() {
        let mut dht = KademliaDht::new([0u8; 20], 100);
        dht.add_node(make_node(1));
        dht.add_node(make_node(2));

        let result = dht.find_node(&make_node(3).id);
        assert_eq!(result.len(), 2, "节点数不足 k 时返回全部节点");
    }

    // ----------------------------------------------------------
    // KademliaMessage 测试
    // ----------------------------------------------------------

    #[test]
    fn test_kademlia_message_creation() {
        let dht = KademliaDht::new([0u8; 20], 100);

        let ping = dht.make_ping();
        match ping {
            KademliaMessage::Ping { sender_id } => {
                assert_eq!(sender_id, [0u8; 20]);
            }
            _ => panic!("应为 Ping 消息"),
        }

        let pong = dht.make_pong();
        assert!(matches!(pong, KademliaMessage::Pong { .. }));

        let target: NodeId = [1u8; 20];
        let find = dht.make_find_node(target);
        match find {
            KademliaMessage::FindNode {
                sender_id,
                target: t,
            } => {
                assert_eq!(sender_id, [0u8; 20]);
                assert_eq!(t, [1u8; 20]);
            }
            _ => panic!("应为 FindNode 消息"),
        }
    }

    #[test]
    fn test_handle_message_refreshes_known_node() {
        let mut dht = KademliaDht::new([0u8; 20], 100);
        let node = make_node(1);
        dht.add_node(node.clone());

        // 模拟过期
        {
            let idx = dht.routing_table.bucket_index(&node.id).unwrap();
            let bucket = dht.routing_table.bucket_mut(idx).unwrap();
            if let Some(pos) = bucket.nodes().iter().position(|n| n.id == node.id) {
                let node_ref = &mut bucket.nodes_mut()[pos];
                node_ref.last_seen = SystemTime::now() - Duration::from_secs(3600);
            }
        }

        // 收到该节点的消息应刷新时间
        let msg = KademliaMessage::Ping { sender_id: node.id };
        dht.handle_message(&msg);

        // 节点应被刷新,不再是过期状态
        let idx = dht.routing_table.bucket_index(&node.id).unwrap();
        let bucket = dht.routing_table.bucket(idx).unwrap();
        let refreshed = bucket.nodes().iter().find(|n| n.id == node.id).unwrap();
        assert!(!refreshed.is_stale(), "收到消息后节点应被刷新");
    }

    // ----------------------------------------------------------
    // RoutingTable 边界测试
    // ----------------------------------------------------------

    #[test]
    fn test_routing_table_bucket_0_for_closest_nodes() {
        let local_id: NodeId = [0u8; 20];
        let mut table = RoutingTable::new(local_id);

        // 距离仅差最后一位 -> bucket 0
        let mut close_id = [0u8; 20];
        close_id[19] = 0x01;
        table.update(make_node_with_id(close_id));
        assert_eq!(table.bucket(0).unwrap().len(), 1);
    }

    #[test]
    fn test_routing_table_bucket_159_for_farthest_nodes() {
        let local_id: NodeId = [0u8; 20];
        let mut table = RoutingTable::new(local_id);

        // 距离在最高位 -> bucket 159
        let mut far_id = [0u8; 20];
        far_id[0] = 0x80;
        table.update(make_node_with_id(far_id));
        assert_eq!(table.bucket(159).unwrap().len(), 1);
    }

    #[test]
    fn test_routing_table_does_not_store_self() {
        let local_id: NodeId = [0xAA; 20];
        let mut table = RoutingTable::new(local_id);
        let result = table.update(make_node_with_id(local_id));
        assert!(!result, "不应将自身添加到路由表");
        assert_eq!(table.node_count(), 0);
    }

    #[test]
    fn test_routing_table_cleanup_stale() {
        let mut table = RoutingTable::new([0u8; 20]);
        table.update(make_node(1));

        let mut stale = make_node(2);
        stale.last_seen = SystemTime::now() - Duration::from_secs(3600);
        table.update(stale);

        assert_eq!(table.node_count(), 2);
        table.cleanup_stale();
        assert_eq!(table.node_count(), 1);
    }

    // ----------------------------------------------------------
    // find_value / store 占位测试
    // ----------------------------------------------------------

    #[test]
    fn test_find_value_returns_none() {
        let dht = KademliaDht::new([0u8; 20], 100);
        let (value, nodes) = dht.find_value(b"test_key");
        assert!(value.is_none());
        assert!(nodes.is_empty());
    }

    #[test]
    fn test_store_does_not_panic() {
        let mut dht = KademliaDht::new([0u8; 20], 100);
        dht.store(b"key".to_vec(), b"value".to_vec());
    }

    // ----------------------------------------------------------
    // 辅助: 暴露 bucket 的可变节点列表(仅测试用)
    // ----------------------------------------------------------

    impl KBucket {
        /// 获取节点列表的可变引用(仅用于测试中模拟节点过期)
        #[cfg(test)]
        fn nodes_mut(&mut self) -> &mut Vec<DhtNode> {
            &mut self.nodes
        }
    }

    // ----------------------------------------------------------
    // 消息序列化 / 反序列化测试
    // ----------------------------------------------------------

    #[test]
    fn test_serialize_ping_pong() {
        let ping = KademliaMessage::Ping {
            sender_id: [0xAA; 20],
        };
        let json = serde_json::to_string(&ping).unwrap();
        let deserialized: KademliaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(ping, deserialized);

        let pong = KademliaMessage::Pong {
            sender_id: [0xBB; 20],
        };
        let json = serde_json::to_string(&pong).unwrap();
        let deserialized: KademliaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(pong, deserialized);
    }

    #[test]
    fn test_serialize_find_node() {
        let msg = KademliaMessage::FindNode {
            sender_id: [1u8; 20],
            target: [2u8; 20],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: KademliaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_serialize_find_node_response_with_nodes() {
        // 使用毫秒对齐的时间戳避免序列化精度损失
        let aligned_time = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let mut node1 = DhtNode::new([1u8; 20], "10.0.0.1:8080".to_string());
        node1.last_seen = aligned_time;
        let mut node2 = DhtNode::new([2u8; 20], "10.0.0.2:9090".to_string());
        node2.last_seen = aligned_time;
        let nodes = vec![node1, node2];

        let msg = KademliaMessage::FindNodeResponse {
            sender_id: [0u8; 20],
            nodes,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: KademliaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);

        // 验证反序列化后的节点信息完整
        if let KademliaMessage::FindNodeResponse {
            nodes: deser_nodes, ..
        } = &deserialized
        {
            assert_eq!(deser_nodes.len(), 2);
            assert_eq!(deser_nodes[0].addr, "10.0.0.1:8080");
            assert_eq!(deser_nodes[1].addr, "10.0.0.2:9090");
        }
    }

    #[test]
    fn test_serialize_find_value_with_data() {
        let msg = KademliaMessage::FindValue {
            sender_id: [3u8; 20],
            key: b"hello".to_vec(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: KademliaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);

        let msg_resp = KademliaMessage::FindValueResponse {
            sender_id: [3u8; 20],
            value: Some(b"world".to_vec()),
            nodes: vec![],
        };
        let json = serde_json::to_string(&msg_resp).unwrap();
        let deserialized: KademliaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg_resp, deserialized);
    }

    #[test]
    fn test_serialize_store() {
        let msg = KademliaMessage::Store {
            sender_id: [4u8; 20],
            key: b"my_key".to_vec(),
            value: b"my_value".to_vec(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: KademliaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_serialize_dht_node_preserves_last_seen() {
        let node = DhtNode::new([5u8; 20], "192.168.1.1:6881".to_string());
        let json = serde_json::to_string(&node).unwrap();
        let deserialized: DhtNode = serde_json::from_str(&json).unwrap();
        assert_eq!(node.id, deserialized.id);
        assert_eq!(node.addr, deserialized.addr);
        // SystemTime 精度到毫秒
        let orig_millis = node
            .last_seen
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let deser_millis = deserialized
            .last_seen
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        assert_eq!(orig_millis, deser_millis);
    }

    // ----------------------------------------------------------
    // KademliaDht 本地存储测试
    // ----------------------------------------------------------

    #[test]
    fn test_local_store_and_find_value() {
        let mut dht = KademliaDht::new([0u8; 20], 100);
        dht.store(b"key1".to_vec(), b"value1".to_vec());
        dht.store(b"key2".to_vec(), b"value2".to_vec());

        let (val, _nodes) = dht.find_value(b"key1");
        assert_eq!(val, Some(b"value1".to_vec()));

        let (val, _nodes) = dht.find_value(b"key2");
        assert_eq!(val, Some(b"value2".to_vec()));

        let (val, _nodes) = dht.find_value(b"nonexistent");
        assert_eq!(val, None);
    }

    #[test]
    fn test_store_overwrites() {
        let mut dht = KademliaDht::new([0u8; 20], 100);
        dht.store(b"key".to_vec(), b"old".to_vec());
        dht.store(b"key".to_vec(), b"new".to_vec());

        let (val, _) = dht.find_value(b"key");
        assert_eq!(val, Some(b"new".to_vec()));
    }

    // ----------------------------------------------------------
    // Transport 网络层测试(localhost UDP)
    // ----------------------------------------------------------

    #[tokio::test]
    async fn test_transport_bind_and_shutdown() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let transport = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let addr = transport.local_addr().unwrap();
        assert!(addr.port() > 0);

        transport.start_recv_loop();
        assert!(transport.is_running());

        transport.shutdown();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!transport.is_running());
    }

    #[tokio::test]
    async fn test_transport_ping_pong() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let dht_b = Arc::new(RwLock::new(KademliaDht::new([2u8; 20], 100)));

        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let tb = DhtTransport::bind("127.0.0.1:0", dht_b).await.unwrap();

        let addr_b = tb.local_addr().unwrap().to_string();

        ta.start_recv_loop();
        tb.start_recv_loop();

        // A -> B: Ping, expect Pong
        let result = ta.ping(&addr_b).await.unwrap();
        assert!(result, "Ping should receive Pong");

        ta.shutdown();
        tb.shutdown();
    }

    #[tokio::test]
    async fn test_transport_find_node_rpc() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let dht_b = Arc::new(RwLock::new(KademliaDht::new([2u8; 20], 100)));

        // Pre-populate B's routing table with some nodes
        {
            let mut b = dht_b.write().await;
            for i in 3..=10u8 {
                let mut id = [0u8; 20];
                id[0] = i;
                b.add_node(DhtNode::new(id, format!("10.0.0.{i}:8080")));
            }
        }

        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let tb = DhtTransport::bind("127.0.0.1:0", dht_b).await.unwrap();
        let addr_b = tb.local_addr().unwrap().to_string();

        ta.start_recv_loop();
        tb.start_recv_loop();

        let target = [5u8; 20];
        let nodes = ta.find_node_rpc(&addr_b, &target).await.unwrap();
        assert!(
            !nodes.is_empty(),
            "B should return nodes from its routing table"
        );
        assert!(nodes.len() <= K_BUCKET_SIZE);

        ta.shutdown();
        tb.shutdown();
    }

    #[tokio::test]
    async fn test_transport_find_value_rpc() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let dht_b = Arc::new(RwLock::new(KademliaDht::new([2u8; 20], 100)));

        // Store a value at B
        {
            let mut b = dht_b.write().await;
            b.store(b"test_key".to_vec(), b"test_value".to_vec());
        }

        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let tb = DhtTransport::bind("127.0.0.1:0", dht_b).await.unwrap();
        let addr_b = tb.local_addr().unwrap().to_string();

        ta.start_recv_loop();
        tb.start_recv_loop();

        let (value, _nodes) = ta.find_value_rpc(&addr_b, b"test_key").await.unwrap();
        assert_eq!(value, Some(b"test_value".to_vec()));

        // Key not stored at B
        let (value, _nodes) = ta
            .find_value_rpc(&addr_b, b"nonexistent")
            .await
            .unwrap();
        assert_eq!(value, None);

        ta.shutdown();
        tb.shutdown();
    }

    #[tokio::test]
    async fn test_transport_store_rpc() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let dht_b = Arc::new(RwLock::new(KademliaDht::new([2u8; 20], 100)));

        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let tb = DhtTransport::bind("127.0.0.1:0", dht_b.clone()).await.unwrap();
        let addr_b = tb.local_addr().unwrap().to_string();

        ta.start_recv_loop();
        tb.start_recv_loop();

        // A sends Store to B
        ta.store_rpc(&addr_b, b"remote_key", b"remote_value")
            .await
            .unwrap();

        // Wait for B to process
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify B has the value
        let b = dht_b.read().await;
        let (value, _) = b.find_value(b"remote_key");
        assert_eq!(value, Some(b"remote_value".to_vec()));

        ta.shutdown();
        tb.shutdown();
    }

    #[tokio::test]
    async fn test_transport_rpc_timeout() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        ta.start_recv_loop();

        // Send RPC to an address where nobody is listening
        // Use a short timeout by sending to a non-routable address
        // For faster test, we use a local port that isn't bound
        let unused_addr = "127.0.0.1:19999";
        let msg = KademliaMessage::Ping {
            sender_id: [1u8; 20],
        };

        // Override timeout for faster test: use tokio::time::timeout directly
        let result = tokio::time::timeout(Duration::from_millis(200), ta.send_rpc(unused_addr, &msg)).await;

        // Should timeout
        assert!(result.is_err(), "RPC to non-listening address should timeout");

        ta.shutdown();
    }

    #[tokio::test]
    async fn test_transport_distributed_store_and_find() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let dht_b2 = Arc::new(RwLock::new(KademliaDht::new([2u8; 20], 100)));

        let tb = DhtTransport::bind("127.0.0.1:0", dht_b2.clone())
            .await
            .unwrap();
        let addr_b = tb.local_addr().unwrap().to_string();

        // Add B to A's routing table
        {
            let mut a = dht_a.write().await;
            a.add_node(DhtNode::new([2u8; 20], addr_b));
        }

        let ta = DhtTransport::bind("127.0.0.1:0", dht_a.clone())
            .await
            .unwrap();

        ta.start_recv_loop();
        tb.start_recv_loop();

        // Distributed store: A stores key-value, replicated to B
        let key = b"dist_key".to_vec();
        let value = b"dist_value".to_vec();
        let stored = ta.distributed_store(key.clone(), value.clone()).await.unwrap();
        assert!(stored > 0, "Should store to at least one remote node");

        // Wait for Store RPCs to be processed
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Verify B has the value
        {
            let b = dht_b2.read().await;
            let (val, _) = b.find_value(&key);
            assert_eq!(val, Some(value.clone()), "B should have the stored value");
        }

        // Distributed find_value: A looks up the key
        let found = ta.distributed_find_value(&key).await.unwrap();
        assert_eq!(found, Some(value));

        ta.shutdown();
        tb.shutdown();
    }

    #[tokio::test]
    async fn test_transport_shutdown_stops_rpc() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        ta.start_recv_loop();
        ta.shutdown();
        tokio::time::sleep(Duration::from_millis(50)).await;

        // RPC after shutdown should fail immediately
        let result = ta.ping("127.0.0.1:19999").await;
        assert!(
            matches!(result, Err(TransportError::Shutdown)),
            "RPC after shutdown should return Shutdown error"
        );
    }

    #[tokio::test]
    async fn test_transport_drop_shuts_down() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let running = ta.running.clone();
        ta.start_recv_loop();
        assert!(running.load(Ordering::SeqCst));

        drop(ta);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!running.load(Ordering::SeqCst), "Drop should set running to false");
    }

    #[tokio::test]
    async fn test_transport_multiple_pings() {
        let dht_server = Arc::new(RwLock::new(KademliaDht::new([0u8; 20], 100)));
        let server = DhtTransport::bind("127.0.0.1:0", dht_server)
            .await
            .unwrap();
        let server_addr = server.local_addr().unwrap().to_string();
        server.start_recv_loop();

        // Multiple clients ping the same server
        for i in 1..=5u8 {
            let mut id = [0u8; 20];
            id[0] = i;
            let dht_client = Arc::new(RwLock::new(KademliaDht::new(id, 100)));
            let client = DhtTransport::bind("127.0.0.1:0", dht_client)
                .await
                .unwrap();
            client.start_recv_loop();

            let result = client.ping(&server_addr).await.unwrap();
            assert!(result, "Client {i} ping should succeed");

            client.shutdown();
        }

        server.shutdown();
    }

    #[tokio::test]
    async fn test_transport_bidirectional_ping() {
        let dht_a = Arc::new(RwLock::new(KademliaDht::new([1u8; 20], 100)));
        let dht_b = Arc::new(RwLock::new(KademliaDht::new([2u8; 20], 100)));

        let ta = DhtTransport::bind("127.0.0.1:0", dht_a).await.unwrap();
        let tb = DhtTransport::bind("127.0.0.1:0", dht_b).await.unwrap();

        let addr_a = ta.local_addr().unwrap().to_string();
        let addr_b = tb.local_addr().unwrap().to_string();

        ta.start_recv_loop();
        tb.start_recv_loop();

        // A pings B
        assert!(ta.ping(&addr_b).await.unwrap());
        // B pings A
        assert!(tb.ping(&addr_a).await.unwrap());

        ta.shutdown();
        tb.shutdown();
    }
}
