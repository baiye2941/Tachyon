//! Kademlia RPC 消息类型和 Ed25519 非对称签名
//!
//! H-3 修复: 将 BLAKE3 对称 MAC 替换为 Ed25519 非对称签名。
//! 旧方案使用 sender_id 作为 BLAKE3 key,攻击者知道 sender_id 即可伪造签名。
//! 新方案每个节点持有 Ed25519 密钥对,NodeId 从公钥派生,签名只能由私钥持有者生成。

use serde::{Deserialize, Serialize};

use super::node::{DhtNode, DhtSignature, NodeId, NodeIdentity};

// ============================================================
// Kademlia RPC 消息
// ============================================================

/// Kademlia 协议消息类型
///
/// 定义了 Kademlia DHT 的四种核心 RPC 消息以及对应的响应。
///
/// # 消息身份认证 (H-3)
/// 通过 [`sign_message`] / [`verify_message_signature`] 提供 Ed25519
/// 非对称签名。每个节点持有唯一的 Ed25519 密钥对:
/// - **签名**: 使用私钥对消息签名,任何持有公钥的节点可验证
/// - **NodeId**: 从公钥派生 (`blake3(public_key)[..20]`),保证不可伪造
///
/// 与旧方案(BLAKE3 keyed hash, key = 公开的 NodeId)不同,
/// Ed25519 签名的私钥仅由节点自身持有,攻击者无法伪造来自其他节点的消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KademliaMessage {
    /// 心跳探测
    Ping {
        sender_id: NodeId,
        /// H-3: Ed25519 非对称签名 (含 64 字节签名 + 32 字节公钥)
        signature: Option<DhtSignature>,
    },
    /// 心跳响应
    Pong {
        sender_id: NodeId,
        signature: Option<DhtSignature>,
    },
    /// 查找距离 target 最近的节点
    FindNode {
        sender_id: NodeId,
        target: NodeId,
        signature: Option<DhtSignature>,
    },
    /// FindNode 响应:返回最近的 k 个节点
    FindNodeResponse {
        sender_id: NodeId,
        nodes: Vec<DhtNode>,
        signature: Option<DhtSignature>,
    },
    /// 按 key 查找存储的值
    FindValue {
        sender_id: NodeId,
        key: Vec<u8>,
        signature: Option<DhtSignature>,
    },
    /// FindValue 响应:找到值则返回值,否则返回最近的 k 个节点
    FindValueResponse {
        sender_id: NodeId,
        value: Option<Vec<u8>>,
        nodes: Vec<DhtNode>,
        signature: Option<DhtSignature>,
    },
    /// 存储键值对到目标节点
    Store {
        sender_id: NodeId,
        key: Vec<u8>,
        value: Vec<u8>,
        signature: Option<DhtSignature>,
    },
}

impl KademliaMessage {
    /// 提取消息中的 sender_id
    pub(crate) fn sender_id(&self) -> Option<&NodeId> {
        match self {
            Self::Ping { sender_id, .. }
            | Self::Pong { sender_id, .. }
            | Self::FindNode { sender_id, .. }
            | Self::FindNodeResponse { sender_id, .. }
            | Self::FindValue { sender_id, .. }
            | Self::FindValueResponse { sender_id, .. }
            | Self::Store { sender_id, .. } => Some(sender_id),
        }
    }

    /// 提取消息中的签名
    pub(crate) fn signature(&self) -> Option<&Option<DhtSignature>> {
        match self {
            Self::Ping { signature, .. }
            | Self::Pong { signature, .. }
            | Self::FindNode { signature, .. }
            | Self::FindNodeResponse { signature, .. }
            | Self::FindValue { signature, .. }
            | Self::FindValueResponse { signature, .. }
            | Self::Store { signature, .. } => Some(signature),
        }
    }

    /// 设置消息签名
    pub(crate) fn set_signature(&mut self, sig: DhtSignature) {
        match self {
            Self::Ping { signature, .. }
            | Self::Pong { signature, .. }
            | Self::FindNode { signature, .. }
            | Self::FindNodeResponse { signature, .. }
            | Self::FindValue { signature, .. }
            | Self::FindValueResponse { signature, .. }
            | Self::Store { signature, .. } => {
                *signature = Some(sig);
            }
        }
    }
}

// ============================================================
// Ed25519 消息签名 (H-3)
// ============================================================

/// 对消息进行 Ed25519 签名
///
/// 使用节点的 Ed25519 私钥签名。签名过程:
/// 1. 构造 signature=None 的消息副本 (避免签名自引用)
/// 2. 将副本序列化为 postcard 二进制格式
/// 3. 使用私钥对序列化字节签名
/// 4. 返回包含签名和公钥的 `DhtSignature`
///
/// 验证方可以:
/// 1. 从 `DhtSignature.public_key` 派生 NodeId,与 `sender_id` 比对
/// 2. 使用公钥验证 Ed25519 签名
pub fn sign_message(identity: &NodeIdentity, msg: &KademliaMessage) -> Option<DhtSignature> {
    // 签名计算时排除 signature 字段:构造一个 signature=None 的副本
    let msg_for_sign = clear_signature(msg);
    let body_bytes = postcard::to_allocvec(&msg_for_sign).unwrap_or_default();
    Some(identity.sign(&body_bytes))
}

/// 验证消息的 Ed25519 签名
///
/// 验证步骤:
/// 1. 从 `signature.public_key` 派生 NodeId,与消息的 `sender_id` 比对
///    (如果公钥与声称的 NodeId 不匹配,说明签名者不是该 NodeId 的真正持有者)
/// 2. 构造 signature=None 的消息副本,序列化后使用公钥验证签名
///
/// 返回 `true` 表示签名有效且公钥与 sender_id 绑定,`false` 表示验证失败。
pub fn verify_message_signature(msg: &KademliaMessage, signature: &DhtSignature) -> bool {
    match msg.sender_id() {
        Some(sender_id) => {
            // 构造排除签名的副本,序列化后验证
            let msg_for_verify = clear_signature(msg);
            let body_bytes = postcard::to_allocvec(&msg_for_verify).unwrap_or_default();
            signature.verify(&body_bytes, sender_id)
        }
        None => false,
    }
}

/// 构造 signature=None 的消息副本,用于签名计算和验证
///
/// 签名和验证都必须排除 signature 字段本身,否则签名会随自身变化,
/// 导致循环依赖。此函数返回一个所有 signature 字段设为 None 的副本。
fn clear_signature(msg: &KademliaMessage) -> KademliaMessage {
    let mut copy = msg.clone();
    match &mut copy {
        KademliaMessage::Ping { signature, .. }
        | KademliaMessage::Pong { signature, .. }
        | KademliaMessage::FindNode { signature, .. }
        | KademliaMessage::FindNodeResponse { signature, .. }
        | KademliaMessage::FindValue { signature, .. }
        | KademliaMessage::FindValueResponse { signature, .. }
        | KademliaMessage::Store { signature, .. } => {
            *signature = None;
        }
    }
    copy
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dht::node::public_key_to_node_id;
    use std::time::{Duration, UNIX_EPOCH};

    // ----------------------------------------------------------
    // 基础消息测试
    // ----------------------------------------------------------

    #[test]
    fn test_kademlia_message_creation() {
        let ping = KademliaMessage::Ping {
            sender_id: [0u8; 20],
            signature: None,
        };
        match ping {
            KademliaMessage::Ping { sender_id, .. } => {
                assert_eq!(sender_id, [0u8; 20]);
            }
            _ => panic!("应为 Ping 消息"),
        }

        let pong = KademliaMessage::Pong {
            sender_id: [0u8; 20],
            signature: None,
        };
        assert!(matches!(pong, KademliaMessage::Pong { .. }));

        let target: NodeId = [1u8; 20];
        let find = KademliaMessage::FindNode {
            sender_id: [0u8; 20],
            target,
            signature: None,
        };
        match find {
            KademliaMessage::FindNode {
                sender_id,
                target: t,
                ..
            } => {
                assert_eq!(sender_id, [0u8; 20]);
                assert_eq!(t, [1u8; 20]);
            }
            _ => panic!("应为 FindNode 消息"),
        }
    }

    // ----------------------------------------------------------
    // 序列化/反序列化测试
    // ----------------------------------------------------------

    #[test]
    fn test_serialize_ping_pong() {
        let ping = KademliaMessage::Ping {
            sender_id: [0xAA; 20],
            signature: None,
        };
        let json = serde_json::to_string(&ping).unwrap();
        let deserialized: KademliaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(ping, deserialized);

        let pong = KademliaMessage::Pong {
            sender_id: [0xBB; 20],
            signature: None,
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
            signature: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: KademliaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    #[test]
    fn test_serialize_find_node_response_with_nodes() {
        let aligned_time = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let mut node1 = DhtNode::new([1u8; 20], "10.0.0.1:8080".to_string());
        node1.last_seen = aligned_time;
        let mut node2 = DhtNode::new([2u8; 20], "10.0.0.2:9090".to_string());
        node2.last_seen = aligned_time;
        let nodes = vec![node1, node2];

        let msg = KademliaMessage::FindNodeResponse {
            sender_id: [0u8; 20],
            nodes,
            signature: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: KademliaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);

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
            signature: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: KademliaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);

        let msg_resp = KademliaMessage::FindValueResponse {
            sender_id: [3u8; 20],
            value: Some(b"world".to_vec()),
            nodes: vec![],
            signature: None,
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
            signature: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: KademliaMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, deserialized);
    }

    // ----------------------------------------------------------
    // H-3: Ed25519 签名/验证测试
    // ----------------------------------------------------------

    #[test]
    fn test_node_identity_generate_valid() {
        let identity = NodeIdentity::generate();
        // NodeId 不应全零(极低概率)
        assert_ne!(identity.node_id(), [0u8; 20]);
        // 公钥不应全零
        assert_ne!(identity.verifying_key().as_bytes(), &[0u8; 32]);
    }

    #[test]
    fn test_node_identity_deterministic_from_seed() {
        let seed = [42u8; 32];
        let a = NodeIdentity::from_seed(&seed);
        let b = NodeIdentity::from_seed(&seed);
        assert_eq!(a.node_id(), b.node_id());
        assert_eq!(a.verifying_key().as_bytes(), b.verifying_key().as_bytes());
    }

    #[test]
    fn test_node_identity_unique() {
        let a = NodeIdentity::generate();
        let b = NodeIdentity::generate();
        assert_ne!(a.node_id(), b.node_id());
    }

    #[test]
    fn test_public_key_to_node_id_consistency() {
        let identity = NodeIdentity::generate();
        let derived = public_key_to_node_id(identity.verifying_key().as_bytes());
        assert_eq!(identity.node_id(), derived);
    }

    #[test]
    fn test_sign_and_verify_success() {
        let identity = NodeIdentity::generate();
        let mut msg = KademliaMessage::Ping {
            sender_id: identity.node_id(),
            signature: None,
        };
        let sig = sign_message(&identity, &msg).expect("签名应成功");
        msg.set_signature(sig.clone());

        // 验证应通过
        assert!(verify_message_signature(&msg, &sig));
    }

    #[test]
    fn test_sign_and_verify_wrong_sender_fails() {
        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        // 用 identity_a 签名,但声称 sender_id 是 b
        let msg = KademliaMessage::Ping {
            sender_id: identity_b.node_id(),
            signature: None,
        };
        let sig = sign_message(&identity_a, &msg).expect("签名应成功");

        // 验证应失败:公钥派生的 NodeId 与 sender_id 不匹配
        assert!(!verify_message_signature(&msg, &sig));
    }

    #[test]
    fn test_tampered_message_fails() {
        let identity = NodeIdentity::generate();
        let msg = KademliaMessage::FindNode {
            sender_id: identity.node_id(),
            target: [0u8; 20],
            signature: None,
        };
        let sig = sign_message(&identity, &msg).expect("签名应成功");

        // 篡改消息的 target 字段
        let tampered = KademliaMessage::FindNode {
            sender_id: identity.node_id(),
            target: [0xFF; 20],
            signature: None,
        };
        // 签名应无法验证被篡改的消息
        assert!(!verify_message_signature(&tampered, &sig));
    }

    #[test]
    fn test_cross_identity_forgery_fails() {
        let identity_a = NodeIdentity::generate();
        let identity_b = NodeIdentity::generate();
        let msg_a = KademliaMessage::Ping {
            sender_id: identity_a.node_id(),
            signature: None,
        };
        let _sig_a = sign_message(&identity_a, &msg_a).expect("签名应成功");

        // identity_b 试图用自己的签名冒充 a
        let msg_b = KademliaMessage::Ping {
            sender_id: identity_a.node_id(), // 声称是 a
            signature: None,
        };
        let sig_b = sign_message(&identity_b, &msg_b).expect("签名应成功");

        // b 的签名无法验证 a 的消息
        assert!(!verify_message_signature(&msg_a, &sig_b));
        // b 签名的消息声称 sender=a,但公钥与 a 的 NodeId 不匹配
        assert!(!verify_message_signature(&msg_b, &sig_b));
    }

    #[test]
    fn test_dht_signature_serialization_roundtrip() {
        let identity = NodeIdentity::generate();
        let msg = KademliaMessage::Ping {
            sender_id: identity.node_id(),
            signature: None,
        };
        let sig = sign_message(&identity, &msg).expect("签名应成功");

        // postcard 二进制序列化/反序列化
        let bytes = postcard::to_allocvec(&sig).unwrap();
        let deser: DhtSignature = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(sig, deser);
    }

    #[test]
    fn test_signed_message_postcard_roundtrip() {
        let identity = NodeIdentity::generate();
        let mut msg = KademliaMessage::Ping {
            sender_id: identity.node_id(),
            signature: None,
        };
        let sig = sign_message(&identity, &msg).expect("签名应成功");
        msg.set_signature(sig.clone());

        // postcard 序列化 → 反序列化
        let bytes = postcard::to_allocvec(&msg).unwrap();
        let deser: KademliaMessage = postcard::from_bytes(&bytes).unwrap();

        // 签名字段应完整保留
        if let KademliaMessage::Ping {
            signature,
            sender_id,
            ..
        } = &deser
        {
            assert!(signature.is_some());
            let deser_sig = signature.as_ref().unwrap();
            // 验证反序列化后的签名仍然有效
            let msg_for_verify = clear_signature(&deser);
            let body_bytes = postcard::to_allocvec(&msg_for_verify).unwrap_or_default();
            assert!(deser_sig.verify(&body_bytes, sender_id));
        } else {
            panic!("反序列化应为 Ping");
        }
    }
}
