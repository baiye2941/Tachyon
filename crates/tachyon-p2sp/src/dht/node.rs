//! DHT 节点类型、XOR 距离度量和密钥身份定义

use std::time::{Duration, SystemTime};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
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
/// M-25 修复: 使用 OS CSPRNG (getrandom) 生成节点 ID,
/// 替代之前基于 SipHash/RandomState 的伪随机方案。
/// DHT 安全性依赖 NodeId 不可预测性,必须使用密码学安全的随机源。
///
/// # Deprecated
/// 推荐使用 [`NodeIdentity::generate()`] 生成密钥对,
/// NodeId 从 Ed25519 公钥派生,支持非对称签名验证。
#[must_use]
pub fn generate_node_id() -> NodeId {
    let mut id = [0u8; 20];
    getrandom::fill(&mut id).expect("OS CSPRNG 不可用,无法生成安全的 NodeId");
    id
}

// ============================================================
// H-3: Ed25519 非对称签名身份
// ============================================================

/// 从 Ed25519 公钥派生 NodeId
///
/// NodeId = BLAKE3(public_key_bytes)[..20]
/// 这保证了 NodeId 与公钥的绑定关系——验证方可以从公钥重新计算 NodeId
/// 并与消息中的 sender_id 比对,防止伪造。
#[must_use]
pub fn public_key_to_node_id(public_key: &[u8; 32]) -> NodeId {
    let hash = blake3::hash(public_key);
    let mut id = [0u8; 20];
    id.copy_from_slice(&hash.as_bytes()[..20]);
    id
}

/// DHT 节点身份：Ed25519 密钥对 + 派生的 NodeId
///
/// H-3 修复: 使用 Ed25519 非对称签名替代 BLAKE3 对称 MAC。
/// 每个 DHT 节点持有唯一的 Ed25519 密钥对:
/// - **签名**: 使用私钥对消息签名,任何持有公钥的节点可验证
/// - **NodeId**: 从公钥派生 (`blake3(public_key)[..20]`),保证不可伪造
///
/// 与旧方案(BLAKE3 keyed hash, key = 公开的 NodeId)不同,
/// Ed25519 签名的私钥仅由节点自身持有,攻击者无法伪造来自其他节点的消息。
pub struct NodeIdentity {
    /// Ed25519 签名私钥
    signing_key: SigningKey,
    /// 派生的 NodeId = blake3(verifying_key)[..20]
    node_id: NodeId,
}

impl NodeIdentity {
    /// 生成随机 Ed25519 密钥对并派生 NodeId
    ///
    /// 使用 OS CSPRNG 生成密钥,保证不可预测性。
    /// NodeId 自动从公钥派生,无需手动指定。
    #[must_use]
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let verifying_key = signing_key.verifying_key();
        let node_id = public_key_to_node_id(verifying_key.as_bytes());
        Self {
            signing_key,
            node_id,
        }
    }

    /// 从 32 字节种子恢复密钥对
    ///
    /// 用于从持久化存储中恢复节点身份。
    /// 相同的种子总是产生相同的密钥对和 NodeId。
    #[must_use]
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(seed);
        let verifying_key = signing_key.verifying_key();
        let node_id = public_key_to_node_id(verifying_key.as_bytes());
        Self {
            signing_key,
            node_id,
        }
    }

    /// 获取节点 ID (160-bit, 从公钥派生)
    #[must_use]
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// 获取签名私钥引用
    #[must_use]
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// 获取验证公钥
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// 对消息签名,返回 Ed25519 签名
    ///
    /// 签名包含 64 字节 Ed25519 签名和 32 字节公钥,
    /// 验证方可以用公钥验证签名并确认 NodeId 绑定。
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> DhtSignature {
        let sig = self.signing_key.sign(message);
        let public_key = *self.signing_key.verifying_key().as_bytes();
        DhtSignature {
            sig: sig.to_bytes(),
            public_key,
        }
    }
}

/// H-3: DHT 消息的 Ed25519 非对称签名
///
/// 包含 Ed25519 签名(64 字节)和签名者的公钥(32 字节)。
/// 公钥随签名传输,验证方可以:
/// 1. 从公钥派生 NodeId,与消息的 sender_id 比对
/// 2. 使用公钥验证签名
///
/// 这保证了消息确实由持有私钥的节点发出,防止身份伪造。
///
/// 序列化使用 postcard 二进制格式,`sig` 和 `public_key` 以字节向量传输。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DhtSignature {
    /// Ed25519 签名 (64 字节)
    pub sig: [u8; 64],
    /// 签名者的 Ed25519 公钥 (32 字节)
    pub public_key: [u8; 32],
}

impl Serialize for DhtSignature {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // 使用两字段的元组序列化,避免 [u8; 64] 的 serde 限制
        (&self.sig[..], &self.public_key[..]).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DhtSignature {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (sig_vec, pk_vec): (Vec<u8>, Vec<u8>) = Deserialize::deserialize(deserializer)?;
        let sig: [u8; 64] = sig_vec
            .try_into()
            .map_err(|_| serde::de::Error::custom("DhtSignature.sig 长度必须为 64 字节"))?;
        let public_key: [u8; 32] = pk_vec
            .try_into()
            .map_err(|_| serde::de::Error::custom("DhtSignature.public_key 长度必须为 32 字节"))?;
        Ok(Self { sig, public_key })
    }
}

impl DhtSignature {
    /// 使用公钥验证签名,并校验 NodeId 绑定
    ///
    /// 验证步骤:
    /// 1. 从 `public_key` 派生 NodeId,与 `expected_sender_id` 比对
    /// 2. 使用 `public_key` 验证 `sig` 对 `message` 的有效性
    ///
    /// 两步都必须通过,防止攻击者用自己的密钥签名并声称是其他节点。
    pub fn verify(&self, message: &[u8], expected_sender_id: &NodeId) -> bool {
        // 步骤 1: 公钥与 NodeId 绑定校验
        let derived_id = public_key_to_node_id(&self.public_key);
        if derived_id != *expected_sender_id {
            return false;
        }

        // 步骤 2: Ed25519 签名验证
        let verifying_key = match VerifyingKey::from_bytes(&self.public_key) {
            Ok(vk) => vk,
            Err(_) => return false,
        };
        let signature = Signature::from_bytes(&self.sig);
        verifying_key.verify(message, &signature).is_ok()
    }
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
