//! KademliaDht 核心逻辑: 节点管理、本地存储、消息构造

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};

use super::kbucket::{RoutingTable, extract_subnet_key};
use super::message::KademliaMessage;
use super::node::{DhtNode, K_BUCKET_SIZE, NUM_BUCKETS, NodeId};

// ============================================================
// Key → NodeId 映射
// ============================================================

/// 将 key 映射为 NodeId(确定性哈希, 固定盐值填充 20 字节)
///
/// 使用固定盐值 + DefaultHasher 保证同一 key 始终映射到同一 NodeId,
/// 这是 Kademlia DHT 正确性的前提: 不同节点必须对同一 key 得到相同的 NodeId。
pub(crate) fn key_to_node_id(key: &[u8]) -> NodeId {
    use std::hash::Hasher;
    let mut id = [0u8; 20];
    for (i, chunk) in id.chunks_mut(8).enumerate() {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        // 固定盐值: 0xDEAD_BEEF_CAFE_BABE + chunk 索引, 保证确定性
        hasher.write_u64(0xDEAD_BEEF_CAFE_BABE_u64.wrapping_add(i as u64));
        hasher.write(key);
        let val = hasher.finish();
        let bytes = val.to_le_bytes();
        for (j, b) in chunk.iter_mut().enumerate() {
            *b = bytes[j];
        }
    }
    id
}

/// 生成指定 bucket 范围内的随机 NodeId (A-10: Bucket Refresh)
///
/// 生成一个与 local_id 的 XOR 距离前导零位数为 (159 - bucket_index) 的随机 ID,
/// 使得该 ID 恰好落入目标 bucket 的键空间范围。
///
/// 例如 bucket_index=159 生成距离为 0b1xxxx... 的 ID(最高位不同),
/// bucket_index=0 生成距离为 0b000...01x...x 的 ID(仅最后一位不同)。
pub(crate) fn generate_random_id_in_bucket_range(local_id: &NodeId, bucket_index: usize) -> NodeId {
    use std::collections::hash_map::RandomState;

    let target_lz = 159 - bucket_index; // 目标前导零位数 (0..159)
    let mut distance = [0u8; 20];

    // 设置目标前导零位后的第一个 1-bit
    let byte_idx = target_lz / 8;
    let bit_idx = target_lz % 8;
    distance[byte_idx] |= 1 << (7 - bit_idx);

    // 用随机数据填充后续字节
    let rb = RandomState::new();
    for i in (byte_idx + 1)..20 {
        let mut hasher = rb.build_hasher();
        hasher.write_u64(i as u64);
        hasher.write_u64(bucket_index as u64);
        distance[i] = hasher.finish() as u8;
    }

    // 随机化目标字节中 1-bit 之后的低位
    if bit_idx < 7 {
        let mut hasher = rb.build_hasher();
        hasher.write_u64(byte_idx as u64);
        hasher.write_u64(bucket_index as u64);
        let rand_byte = hasher.finish() as u8;
        let mask = (1u8 << (7 - bit_idx)) - 1;
        distance[byte_idx] |= rand_byte & mask;
    }

    // XOR distance 还原为 NodeId
    let mut id = [0u8; 20];
    for i in 0..20 {
        id[i] = local_id[i] ^ distance[i];
    }
    id
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
    ///
    /// # Sybil 防护 (S-12)
    /// 限制同一 /24 子网在路由表中最多 2 个节点,
    /// 防止攻击者从同一 C 段批量生成节点劫持查找。
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

        // Sybil 防护: 检查同一 /24 子网的节点数量
        const MAX_NODES_PER_SUBNET: usize = 2;
        if let Some(subnet_key) = extract_subnet_key(&node.addr) {
            let subnet_count = self.routing_table.count_nodes_in_subnet(&subnet_key);
            if subnet_count >= MAX_NODES_PER_SUBNET {
                tracing::debug!(
                    addr = %node.addr,
                    subnet = %subnet_key,
                    count = subnet_count,
                    "Sybil 防护: /24 子网节点数已达上限,拒绝插入"
                );
                return;
            }
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
        let nodes = self
            .routing_table
            .find_closest(&key_to_node_id(key), K_BUCKET_SIZE);
        (value, nodes)
    }

    /// 本地存储查找(不触发网络操作)
    pub fn find_value_local(&self, key: &[u8]) -> (Option<Vec<u8>>, Vec<DhtNode>) {
        let value = self.local_store.get(key).cloned();
        let nodes = self
            .routing_table
            .find_closest(&key_to_node_id(key), K_BUCKET_SIZE);
        (value, nodes)
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

    /// Bucket Refresh: 对指定 bucket 执行一次刷新查找 (A-10)
    ///
    /// 生成一个落入目标 bucket 键空间的随机 NodeId,
    /// 然后在本地路由表中查找最近的 k 个节点。
    /// 传输层会将这些节点作为迭代查找的初始候选集发送到网络。
    ///
    /// Kademlia 规范要求定期刷新不活跃的 bucket,
    /// 以确保路由表中低活跃度区域不会因节点离线而变空。
    pub fn refresh_bucket(&self, bucket_index: usize) -> Vec<DhtNode> {
        let random_id = generate_random_id_in_bucket_range(&self.local_id, bucket_index);
        self.routing_table.find_closest(&random_id, K_BUCKET_SIZE)
    }
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn make_node(id_byte: u8) -> DhtNode {
        let mut id = [0u8; 20];
        id[0] = id_byte;
        // 使用不同 /24 子网避免触发 Sybil 防护(同一子网最多 2 个节点)
        DhtNode::new(id, format!("10.0.{id_byte}.1:8080"))
    }

    // ----------------------------------------------------------
    // KademliaDht 测试
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
        // 结果中的第一个应该是 target 自身(已添加到路由表)
        assert_eq!(result[0].id, target);
        // 结果不超过 K_BUCKET_SIZE
        assert!(result.len() <= K_BUCKET_SIZE);
    }

    #[test]
    fn test_find_node_monotonic_closeness() {
        use super::super::node::xor_distance;

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
    // handle_message 测试
    // ----------------------------------------------------------

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
    // find_value / store 测试
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
    // key_to_node_id 确定性哈希测试 (F-07 修复验证)
    // ----------------------------------------------------------

    #[test]
    fn test_key_to_node_id_deterministic() {
        // 同一 key 多次调用必须返回相同 NodeId
        let key = b"hello_tachyon";
        let id1 = key_to_node_id(key);
        let id2 = key_to_node_id(key);
        let id3 = key_to_node_id(key);
        assert_eq!(id1, id2, "key_to_node_id 必须确定性: 第1次 != 第2次");
        assert_eq!(id2, id3, "key_to_node_id 必须确定性: 第2次 != 第3次");
    }

    #[test]
    fn test_key_to_node_id_different_keys() {
        // 不同 key 应当产生不同 NodeId (极低碰撞概率)
        let id_a = key_to_node_id(b"key_alpha");
        let id_b = key_to_node_id(b"key_beta");
        assert_ne!(id_a, id_b, "不同 key 不应产生相同 NodeId");
    }

    #[test]
    fn test_key_to_node_id_covers_all_bytes() {
        // 验证 NodeId 的全部 20 字节都被填充, 非零部分覆盖充分
        let id = key_to_node_id(b"coverage_test_key");
        // 至少 10/20 字节应为非零 (极宽松的统计下限)
        let nonzero = id.iter().filter(|&&b| b != 0).count();
        assert!(
            nonzero >= 10,
            "NodeId 非零字节过少: {nonzero}/20, 哈希分布可能异常"
        );
    }

    // ----------------------------------------------------------
    // A-10: Bucket Refresh 测试
    // ----------------------------------------------------------

    #[test]
    fn test_generate_random_id_in_bucket_range() {
        use super::super::node::leading_zeros;

        let local_id = [0u8; 20];

        // 对每个 bucket 生成随机 ID,验证其落入正确的 bucket
        for bucket_idx in [0, 1, 50, 100, 158, 159] {
            let random_id = generate_random_id_in_bucket_range(&local_id, bucket_idx);
            let dist = super::super::node::xor_distance(&local_id, &random_id);
            let lz = leading_zeros(&dist);
            let actual_bucket = 159 - lz as usize;
            assert_eq!(
                actual_bucket, bucket_idx,
                "bucket {bucket_idx}: 生成的随机 ID 应落入该 bucket, 实际落入 bucket {actual_bucket} (lz={lz})"
            );
        }
    }

    #[test]
    fn test_generate_random_id_not_self() {
        let local_id = [0xAA; 20];
        // 生成的随机 ID 不应与 local_id 相同 (距离不为零)
        for bucket_idx in 0..NUM_BUCKETS {
            let random_id = generate_random_id_in_bucket_range(&local_id, bucket_idx);
            assert_ne!(
                random_id, local_id,
                "bucket {bucket_idx}: 随机 ID 不应等于 local_id"
            );
        }
    }

    #[test]
    fn test_refresh_bucket_with_nodes() {
        let mut dht = KademliaDht::new([0u8; 20], 1000);
        // 添加一些节点
        for i in 1..=50u8 {
            dht.add_node(make_node(i));
        }
        // 对每个非空 bucket 执行刷新
        let non_empty: Vec<usize> = (0..NUM_BUCKETS)
            .filter(|&i| {
                dht.routing_table()
                    .bucket(i)
                    .map_or(false, |b| !b.is_empty())
            })
            .collect();
        assert!(!non_empty.is_empty(), "应有非空 bucket");

        for &bucket_idx in &non_empty {
            let result = dht.refresh_bucket(bucket_idx);
            // 刷新应返回一些候选节点(来自路由表)
            assert!(
                !result.is_empty() || dht.node_count() == 0,
                "bucket {bucket_idx}: refresh 应返回候选节点"
            );
        }
    }

    #[test]
    fn test_kbucket_last_activity_updated_on_insert() {
        use super::super::kbucket::KBucket;

        let mut bucket = KBucket::new();
        let before = bucket.last_activity();
        // 短暂等待以确保时间戳不同
        std::thread::sleep(std::time::Duration::from_millis(1));
        let node = DhtNode::new([1u8; 20], "10.0.0.1:8080".to_string());
        bucket.update(node);
        let after = bucket.last_activity();
        assert!(after > before, "插入节点后 last_activity 应被刷新");
    }
}
