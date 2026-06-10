//! DHT 节点类型、XOR 距离度量和常量定义

use std::hash::{BuildHasher, Hasher};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

// ============================================================
// 常量
// ============================================================

/// k-bucket 容量(Kademlia 标准 k 值)
pub const K_BUCKET_SIZE: usize = 20;

/// 并发查找因子(迭代查找时每轮并发请求数)
pub const ALPHA: usize = 3;

/// NodeId 位数(160-bit)
pub(crate) const NODE_ID_BITS: usize = 160;

/// k-bucket 数量(等于 NodeId 位数)
pub(crate) const NUM_BUCKETS: usize = NODE_ID_BITS;

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
// Serde 辅助: SystemTime <-> UNIX 毫秒时间戳
// ============================================================

/// `SystemTime` 的 serde 序列化辅助模块(以 UNIX 毫秒时间戳存储)
pub(crate) mod serde_system_time {
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
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_node_not_stale_initially() {
        let mut id = [0u8; 20];
        id[0] = 1;
        let node = DhtNode::new(id, format!("192.168.1.1:8080"));
        assert!(!node.is_stale());
    }

    #[test]
    fn test_serialize_dht_node_preserves_last_seen() {
        use std::time::UNIX_EPOCH;
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
}
