//! Kademlia DHT 实现
//!
//! 基于 Kademlia 协议的分布式哈希表,用于 Peer 发现。
//! 包含 XOR 距离度量、k-bucket 路由表、迭代查找算法。

use std::hash::{BuildHasher, Hasher};
use std::time::{Duration, SystemTime};

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhtNode {
    /// 节点 ID
    pub id: NodeId,
    /// 节点地址
    pub addr: String,
    /// 最后通信时间
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

impl KademliaDht {
    /// 创建新的 DHT 实例
    pub fn new(local_id: NodeId, max_nodes: usize) -> Self {
        Self {
            local_id,
            routing_table: RoutingTable::new(local_id),
            max_nodes,
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

    /// 按 key 查找存储的值
    ///
    /// 当前版本为占位实现,返回 None + 最近节点列表。
    /// 完整的分布式存储将在后续版本中实现。
    pub fn find_value(&self, _key: &[u8]) -> (Option<Vec<u8>>, Vec<DhtNode>) {
        // TODO: 实现 DHT 分布式存储(需要 RPC 通信)
        (None, Vec::new())
    }

    /// 存储键值对
    ///
    /// 当前版本为占位实现。
    /// 完整的分布式存储将在后续版本中实现。
    pub fn store(&mut self, _key: Vec<u8>, _value: Vec<u8>) {
        // TODO: 实现 DHT 分布式存储(需要 RPC 通信)
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
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

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
}
