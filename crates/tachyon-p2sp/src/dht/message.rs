//! Kademlia RPC 消息类型和 BLAKE3 完整性校验

use serde::{Deserialize, Serialize};

use super::node::{DhtNode, NodeId};

// ============================================================
// Kademlia RPC 消息
// ============================================================

/// Kademlia 协议消息类型
///
/// 定义了 Kademlia DHT 的四种核心 RPC 消息以及对应的响应。
/// 实际的网络发送/接收将在后续版本中通过 tachyon-protocol 实现。
///
/// # 消息完整性 (S-11)
/// 通过 [`sign_message`] / [`verify_message_signature`] 提供可选的
/// BLAKE3 keyed hash 完整性校验。每个节点用自身 NodeId 作为 key
/// 对序列化消息体签名。
/// TODO: 升级为 Ed25519 非对称签名以实现真正的身份认证。
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

impl KademliaMessage {
    /// 提取消息中的 sender_id
    pub(crate) fn sender_id(&self) -> Option<&NodeId> {
        match self {
            Self::Ping { sender_id }
            | Self::Pong { sender_id }
            | Self::FindNode { sender_id, .. }
            | Self::FindNodeResponse { sender_id, .. }
            | Self::FindValue { sender_id, .. }
            | Self::FindValueResponse { sender_id, .. }
            | Self::Store { sender_id, .. } => Some(sender_id),
        }
    }
}

// ============================================================
// BLAKE3 消息签名
// ============================================================

/// 对消息进行 BLAKE3 keyed hash 签名
///
/// 使用 sender 的 NodeId (扩展到 32 字节) 作为 key,
/// 对消息序列化结果进行 keyed hash,返回 32 字节签名。
pub fn sign_message(msg: &KademliaMessage) -> Option<[u8; 32]> {
    let sender_id = msg.sender_id()?;
    let body_bytes = serde_json::to_vec(msg).unwrap_or_default();
    let key = node_id_to_blake3_key(sender_id);
    let hash = blake3::keyed_hash(&key, &body_bytes);
    Some(*hash.as_bytes())
}

/// 验证消息签名是否与声称的 sender_id 匹配
///
/// `signature` 为 `sign_message` 返回的 32 字节 hash。
/// 如果签名有效返回 `true`,否则返回 `false`。
pub fn verify_message_signature(msg: &KademliaMessage, signature: &[u8; 32]) -> bool {
    match msg.sender_id() {
        Some(sender_id) => {
            let body_bytes = serde_json::to_vec(msg).unwrap_or_default();
            let key = node_id_to_blake3_key(sender_id);
            let expected = blake3::keyed_hash(&key, &body_bytes);
            signature == expected.as_bytes()
        }
        None => false,
    }
}

/// 将 20 字节 NodeId 扩展为 32 字节 BLAKE3 key (后 12 字节填零)
fn node_id_to_blake3_key(id: &NodeId) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..20].copy_from_slice(id);
    key
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn test_kademlia_message_creation() {
        let ping = KademliaMessage::Ping {
            sender_id: [0u8; 20],
        };
        match ping {
            KademliaMessage::Ping { sender_id } => {
                assert_eq!(sender_id, [0u8; 20]);
            }
            _ => panic!("应为 Ping 消息"),
        }

        let pong = KademliaMessage::Pong {
            sender_id: [0u8; 20],
        };
        assert!(matches!(pong, KademliaMessage::Pong { .. }));

        let target: NodeId = [1u8; 20];
        let find = KademliaMessage::FindNode {
            sender_id: [0u8; 20],
            target,
        };
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
}
