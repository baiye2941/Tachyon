//! 事件类型定义
//!
//! F-28:`DownloadEvent` 与 `FragmentEvent` 已移除 —— 它们无任何生产消费者
//! (生产进度走 `ProgressEvent` + `watch` 通道,分片进度走 `FragmentProgress`),
//! 仅被本文件的自我测试构造验证,属死代码。
//! 保留 `PeerEvent`,为 P2SP(tachyon-p2sp)特性预留。

use serde::{Deserialize, Serialize};

use crate::types::TaskId;

/// Peer 事件(P2SP)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerEvent {
    /// 发现新 Peer
    Discovered { task_id: TaskId, peer_addr: String },
    /// Peer 连接建立
    Connected { task_id: TaskId, peer_addr: String },
    /// Peer 连接断开
    Disconnected {
        task_id: TaskId,
        peer_addr: String,
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_event_serde_roundtrip() {
        let task_id = TaskId::new_v4();
        let event = PeerEvent::Connected {
            task_id,
            peer_addr: "127.0.0.1:8080".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: PeerEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }
}
