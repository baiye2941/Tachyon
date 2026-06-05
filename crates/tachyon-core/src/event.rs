//! 事件类型定义

use serde::{Deserialize, Serialize};

use crate::types::{DownloadState, TaskId};

/// 下载任务事件
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadEvent {
    /// 任务状态变更
    StateChanged {
        task_id: TaskId,
        old_state: DownloadState,
        new_state: DownloadState,
    },
    /// 进度更新
    Progress {
        task_id: TaskId,
        /// 已下载字节数
        downloaded: u64,
        /// 总字节数
        total: Option<u64>,
        /// 当前速度(字节/秒)
        speed: u64,
    },
    /// 任务完成
    Completed {
        task_id: TaskId,
        /// 文件路径
        file_path: String,
        /// 平均速度(字节/秒)
        avg_speed: u64,
        /// 耗时(毫秒)
        elapsed_ms: u64,
    },
    /// 任务失败
    Failed {
        task_id: TaskId,
        /// 错误信息
        error: String,
    },
}

/// 分片事件
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FragmentEvent {
    /// 分片开始下载
    Started {
        task_id: TaskId,
        fragment_index: u32,
    },
    /// 分片进度
    Progress {
        task_id: TaskId,
        fragment_index: u32,
        downloaded: u64,
        total: u64,
    },
    /// 分片完成
    Completed {
        task_id: TaskId,
        fragment_index: u32,
    },
    /// 分片失败(将重试)
    Failed {
        task_id: TaskId,
        fragment_index: u32,
        error: String,
        retry_count: u32,
    },
}

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
    fn test_download_state_changed_event() {
        let task_id = TaskId::new_v4();
        let event = DownloadEvent::StateChanged {
            task_id,
            old_state: DownloadState::Pending,
            new_state: DownloadState::Downloading,
        };
        match event {
            DownloadEvent::StateChanged {
                old_state,
                new_state,
                ..
            } => {
                assert_eq!(old_state, DownloadState::Pending);
                assert_eq!(new_state, DownloadState::Downloading);
            }
            _ => panic!("错误的事件类型"),
        }
    }

    #[test]
    fn test_download_progress_event() {
        let event = DownloadEvent::Progress {
            task_id: TaskId::new_v4(),
            downloaded: 512,
            total: Some(1024),
            speed: 1024 * 100,
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: DownloadEvent = serde_json::from_str(&json).unwrap();
        match deserialized {
            DownloadEvent::Progress {
                downloaded, total, ..
            } => {
                assert_eq!(downloaded, 512);
                assert_eq!(total, Some(1024));
            }
            _ => panic!("错误的事件类型"),
        }
    }

    #[test]
    fn test_download_completed_event() {
        let event = DownloadEvent::Completed {
            task_id: TaskId::new_v4(),
            file_path: "/tmp/file.bin".into(),
            avg_speed: 1024 * 1024,
            elapsed_ms: 5000,
        };
        match event {
            DownloadEvent::Completed { avg_speed, .. } => {
                assert_eq!(avg_speed, 1024 * 1024);
            }
            _ => panic!("错误的事件类型"),
        }
    }

    #[test]
    fn test_download_failed_event() {
        let event = DownloadEvent::Failed {
            task_id: TaskId::new_v4(),
            error: "网络断开".into(),
        };
        match event {
            DownloadEvent::Failed { error, .. } => {
                assert_eq!(error, "网络断开");
            }
            _ => panic!("错误的事件类型"),
        }
    }

    #[test]
    fn test_fragment_started_event() {
        let event = FragmentEvent::Started {
            task_id: TaskId::new_v4(),
            fragment_index: 0,
        };
        match event {
            FragmentEvent::Started { fragment_index, .. } => {
                assert_eq!(fragment_index, 0);
            }
            _ => panic!("错误的事件类型"),
        }
    }

    #[test]
    fn test_fragment_progress_event() {
        let event = FragmentEvent::Progress {
            task_id: TaskId::new_v4(),
            fragment_index: 1,
            downloaded: 256,
            total: 512,
        };
        let json = serde_json::to_string(&event).unwrap();
        let _: FragmentEvent = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn test_fragment_failed_event_with_retry() {
        let event = FragmentEvent::Failed {
            task_id: TaskId::new_v4(),
            fragment_index: 2,
            error: "连接重置".into(),
            retry_count: 1,
        };
        match event {
            FragmentEvent::Failed {
                retry_count, error, ..
            } => {
                assert_eq!(retry_count, 1);
                assert_eq!(error, "连接重置");
            }
            _ => panic!("错误的事件类型"),
        }
    }

    #[test]
    fn test_peer_events() {
        let task_id = TaskId::new_v4();
        let peer_addr = "192.168.1.100:8080".to_string();

        let discovered = PeerEvent::Discovered {
            task_id,
            peer_addr: peer_addr.clone(),
        };
        let connected = PeerEvent::Connected {
            task_id,
            peer_addr: peer_addr.clone(),
        };
        let disconnected = PeerEvent::Disconnected {
            task_id,
            peer_addr,
            reason: "超时".into(),
        };

        // 验证序列化/反序列化
        let json = serde_json::to_string(&discovered).unwrap();
        let _: PeerEvent = serde_json::from_str(&json).unwrap();
        let json = serde_json::to_string(&connected).unwrap();
        let _: PeerEvent = serde_json::from_str(&json).unwrap();
        let json = serde_json::to_string(&disconnected).unwrap();
        let _: PeerEvent = serde_json::from_str(&json).unwrap();
    }
}
