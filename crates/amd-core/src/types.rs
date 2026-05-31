//! 核心标识类型

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 任务唯一标识
pub type TaskId = Uuid;

/// 下载任务状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DownloadState {
    #[default]
    Pending,
    Connecting,
    Downloading,
    Paused,
    Resuming,
    Verifying,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for DownloadState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadState::Pending => write!(f, "pending"),
            DownloadState::Connecting => write!(f, "connecting"),
            DownloadState::Downloading => write!(f, "downloading"),
            DownloadState::Paused => write!(f, "paused"),
            DownloadState::Resuming => write!(f, "resuming"),
            DownloadState::Verifying => write!(f, "verifying"),
            DownloadState::Completed => write!(f, "completed"),
            DownloadState::Failed => write!(f, "failed"),
            DownloadState::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl DownloadState {
    pub fn try_transition(&self, next: DownloadState) -> Result<DownloadState, String> {
        use DownloadState::*;
        let valid = matches!(
            (self, &next),
            (Pending, Connecting)
                | (Connecting, Downloading)
                | (Connecting, Failed)
                | (Connecting, Cancelled)
                | (Downloading, Paused)
                | (Downloading, Verifying)
                | (Downloading, Failed)
                | (Downloading, Cancelled)
                | (Paused, Resuming)
                | (Paused, Cancelled)
                | (Resuming, Downloading)
                | (Resuming, Failed)
                | (Resuming, Cancelled)
                | (Verifying, Completed)
                | (Verifying, Failed)
                | (Verifying, Cancelled)
                | (Failed, Pending)
                | (Cancelled, Pending)
        );
        if valid {
            Ok(next)
        } else {
            Err(format!("非法状态转换: {self:?} -> {next:?}"))
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            DownloadState::Completed | DownloadState::Failed | DownloadState::Cancelled
        )
    }
}

/// 文件元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    /// 文件名
    pub file_name: String,
    /// 文件大小(字节),None 表示服务端未返回 Content-Length
    pub file_size: Option<u64>,
    /// MIME 类型
    pub content_type: Option<String>,
    /// 支持分片下载
    pub supports_range: bool,
    /// ETag
    pub etag: Option<String>,
    /// 最后修改时间
    pub last_modified: Option<String>,
}

/// 分片信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentInfo {
    /// 分片索引
    pub index: u32,
    /// 起始字节偏移
    pub start: u64,
    /// 结束字节偏移(含)
    pub end: u64,
    /// 分片大小(字节)
    pub size: u64,
    /// 下载进度(已下载字节数)
    pub downloaded: u64,
    /// 分片校验哈希
    pub hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskProgress {
    pub downloaded: u64,
    pub speed: u64,
    /// 进度百分比(0.0 ~ 1.0)
    pub progress: f64,
    pub fragments_done: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadStateChange {
    pub task_id: String,
    pub new_state: DownloadState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_download_state_variants() {
        assert_ne!(DownloadState::Pending, DownloadState::Downloading);
        assert_ne!(DownloadState::Completed, DownloadState::Failed);
        assert_eq!(DownloadState::Paused, DownloadState::Paused);
    }

    #[test]
    fn test_download_state_clone() {
        let state = DownloadState::Downloading;
        let cloned = state;
        assert_eq!(state, cloned);
    }

    #[test]
    fn test_file_metadata_with_size() {
        let meta = FileMetadata {
            file_name: "test.bin".into(),
            file_size: Some(1024),
            content_type: Some("application/octet-stream".into()),
            supports_range: true,
            etag: Some("\"abc\"".into()),
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
        };
        assert_eq!(meta.file_size, Some(1024));
        assert!(meta.supports_range);
    }

    #[test]
    fn test_file_metadata_unknown_size() {
        let meta = FileMetadata {
            file_name: "stream.mp4".into(),
            file_size: None,
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
        };
        assert!(meta.file_size.is_none());
        assert!(!meta.supports_range);
    }

    #[test]
    fn test_fragment_info() {
        let frag = FragmentInfo {
            index: 0,
            start: 0,
            end: 999,
            size: 1000,
            downloaded: 500,
            hash: None,
        };
        assert_eq!(frag.index, 0);
        assert_eq!(frag.size, 1000);
        assert_eq!(frag.downloaded, 500);
    }

    #[test]
    fn test_task_id_generation() {
        let id1 = TaskId::new_v4();
        let id2 = TaskId::new_v4();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_file_metadata_serialization() {
        let meta = FileMetadata {
            file_name: "test.bin".into(),
            file_size: Some(1024),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let deserialized: FileMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.file_name, "test.bin");
        assert_eq!(deserialized.file_size, Some(1024));
    }

    #[test]
    fn test_download_state_serialization() {
        let state = DownloadState::Downloading;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"downloading\"");
        let deserialized: DownloadState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, DownloadState::Downloading);
    }

    #[test]
    fn test_try_transition_valid_paths() {
        use DownloadState::*;
        let valid = [
            (Pending, Connecting),
            (Connecting, Downloading),
            (Connecting, Failed),
            (Connecting, Cancelled),
            (Downloading, Paused),
            (Downloading, Verifying),
            (Downloading, Failed),
            (Downloading, Cancelled),
            (Paused, Resuming),
            (Paused, Cancelled),
            (Resuming, Downloading),
            (Resuming, Failed),
            (Resuming, Cancelled),
            (Verifying, Completed),
            (Verifying, Failed),
            (Verifying, Cancelled),
            (Failed, Pending),
            (Cancelled, Pending),
        ];
        for (from, to) in valid {
            assert!(
                from.try_transition(to).is_ok(),
                "合法转换应成功: {from:?} -> {to:?}"
            );
        }
    }

    #[test]
    fn test_try_transition_invalid_paths() {
        use DownloadState::*;
        let invalid = [
            (Pending, Completed),
            (Pending, Downloading),
            (Completed, Pending),
            (Completed, Failed),
            (Downloading, Pending),
            (Failed, Downloading),
            (Paused, Downloading),
        ];
        for (from, to) in invalid {
            assert!(
                from.try_transition(to).is_err(),
                "非法转换应被拒绝: {from:?} -> {to:?}"
            );
        }
    }

    #[test]
    fn test_is_terminal() {
        use DownloadState::*;
        assert!(!Pending.is_terminal());
        assert!(!Connecting.is_terminal());
        assert!(!Downloading.is_terminal());
        assert!(!Paused.is_terminal());
        assert!(!Resuming.is_terminal());
        assert!(!Verifying.is_terminal());
        assert!(Completed.is_terminal());
        assert!(Failed.is_terminal());
        assert!(Cancelled.is_terminal());
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// 分片 size 应始终等于 end - start + 1
        #[test]
        fn test_fragment_info_size_consistency(
            index in 0u32..1000,
            start in 0u64..u64::MAX / 2,
            size in 1u64..1024 * 1024 * 1024,
        ) {
            let end = start + size - 1;
            let frag = FragmentInfo {
                index,
                start,
                end,
                size,
                downloaded: 0,
                hash: None,
            };
            // 核心不变量: size == end - start + 1
            prop_assert_eq!(frag.size, frag.end - frag.start + 1);
            // end >= start（单字节分片时 end == start）
            prop_assert!(frag.end >= frag.start);
            // size 至少为 1
            prop_assert!(frag.size >= 1);
        }

        /// DownloadState 序列化/反序列化往返保持不变
        #[test]
        fn test_download_state_roundtrip(state in prop_oneof![
            Just(DownloadState::Pending),
            Just(DownloadState::Connecting),
            Just(DownloadState::Downloading),
            Just(DownloadState::Paused),
            Just(DownloadState::Resuming),
            Just(DownloadState::Verifying),
            Just(DownloadState::Completed),
            Just(DownloadState::Failed),
            Just(DownloadState::Cancelled),
        ]) {
            let json = serde_json::to_string(&state).unwrap();
            let deserialized: DownloadState = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(state, deserialized);
        }

        /// FileMetadata 序列化/反序列化往返保持关键字段一致
        #[test]
        fn test_file_metadata_roundtrip(
            file_name in "[a-zA-Z0-9_\\-]{1,50}",
            file_size in prop::option::of(0u64..1024 * 1024 * 1024),
            supports_range in proptest::bool::ANY,
        ) {
            let meta = FileMetadata {
                file_name: file_name.clone(),
                file_size,
                content_type: Some("application/octet-stream".into()),
                supports_range,
                etag: None,
                last_modified: None,
            };
            let json = serde_json::to_string(&meta).unwrap();
            let deserialized: FileMetadata = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(deserialized.file_name, file_name);
            prop_assert_eq!(deserialized.file_size, file_size);
            prop_assert_eq!(deserialized.supports_range, supports_range);
        }

        /// FragmentInfo downloaded 不应超过 size
        #[test]
        fn test_fragment_downloaded_le_size(
            size in 1u64..1024 * 1024,
            downloaded in 0u64..1024 * 1024,
        ) {
            let clamped_downloaded = downloaded.min(size);
            let frag = FragmentInfo {
                index: 0,
                start: 0,
                end: size - 1,
                size,
                downloaded: clamped_downloaded,
                hash: None,
            };
            prop_assert!(frag.downloaded <= frag.size);
        }
    }
}
