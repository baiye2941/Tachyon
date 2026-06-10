//! Kademlia k-bucket 路由表和 Sybil 防护

use std::time::Instant;

use super::node::{DhtNode, K_BUCKET_SIZE, NUM_BUCKETS, NodeId, leading_zeros, xor_distance};

// ============================================================
// 子网提取(Sybil 防护)
// ============================================================

/// 从节点地址中提取 /24 子网键 (IPv4) 或 /48 子网键 (IPv6)
pub(crate) fn extract_subnet_key(addr: &str) -> Option<String> {
    // 从 "ip:port" 中提取 IP 部分
    let ip_str = addr.rsplit_once(':').map(|(ip, _)| ip).unwrap_or(addr);
    // 去掉 IPv6 方括号
    let ip_str = ip_str.trim_start_matches('[').trim_end_matches(']');

    if let Ok(ip) = ip_str.parse::<std::net::IpAddr>() {
        match ip {
            std::net::IpAddr::V4(v4) => {
                let octets = v4.octets();
                Some(format!("{}.{}.{}", octets[0], octets[1], octets[2]))
            }
            std::net::IpAddr::V6(v6) => {
                let segments = v6.segments();
                Some(format!(
                    "{:x}:{:x}:{:x}",
                    segments[0], segments[1], segments[2]
                ))
            }
        }
    } else {
        None // 域名无法提取子网,跳过检查
    }
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
    /// 最后一次活动时间(用于 Bucket Refresh 判定)
    last_activity: Instant,
}

impl KBucket {
    /// 创建空的 k-bucket
    pub fn new() -> Self {
        Self {
            nodes: Vec::with_capacity(K_BUCKET_SIZE),
            replacement_cache: Vec::new(),
            last_activity: Instant::now(),
        }
    }

    /// 获取最后一次活动时间
    pub fn last_activity(&self) -> Instant {
        self.last_activity
    }

    /// 刷新活动时间戳为当前时刻
    pub fn touch_activity(&mut self) {
        self.last_activity = Instant::now();
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
        self.last_activity = Instant::now();
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
            self.last_activity = Instant::now();
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
                self.last_activity = Instant::now();
                self.nodes[0] = replacement;
            }
        }
    }

    /// 获取节点列表的可变引用(仅用于测试中模拟节点过期)
    #[cfg(test)]
    pub(crate) fn nodes_mut(&mut self) -> &mut Vec<DhtNode> {
        &mut self.nodes
    }

    /// 清理 bucket 中的过期节点,并从替换缓存补充
    pub fn evict_stale(&mut self) {
        let before = self.nodes.len();
        self.nodes.retain(|n| !n.is_stale());
        if self.nodes.len() != before {
            self.last_activity = Instant::now();
        }
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

    /// 统计路由表中属于同一子网的节点数量
    pub(crate) fn count_nodes_in_subnet(&self, subnet_key: &str) -> usize {
        let mut count = 0;
        for i in 0..NUM_BUCKETS {
            if let Some(bucket) = self.bucket(i) {
                for node in bucket.nodes().iter() {
                    if let Some(node_subnet) = extract_subnet_key(&node.addr) {
                        if node_subnet == subnet_key {
                            count += 1;
                        }
                    }
                }
            }
        }
        count
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
        DhtNode::new(id, format!("192.168.1.{id_byte}:8080"))
    }

    fn make_node_with_id(id: NodeId) -> DhtNode {
        DhtNode::new(id, "127.0.0.1:8080".to_string())
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
}
