//! 统一错误类型

use thiserror::Error;

/// Tachyon 全局错误类型
#[derive(Error, Debug)]
pub enum DownloadError {
    #[error("网络错误: {0}")]
    Network(String),

    #[error("协议错误: {0}")]
    Protocol(String),

    #[error("I/O 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("分片错误: {0}")]
    Fragment(String),

    #[error("校验失败: 预期 {expected}, 实际 {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("配置错误: {0}")]
    Config(String),

    #[error("任务已取消")]
    Cancelled,

    #[error("任务不存在: {0}")]
    TaskNotFound(String),

    #[error("连接池已耗尽")]
    ConnectionPoolExhausted,

    #[error("超时: {0}")]
    Timeout(String),

    /// 服务端限流(HTTP 429/503)。
    ///
    /// `retry_after_secs` 来自 `Retry-After` 响应头(若服务端提供),
    /// 重试循环应据此延长退避;无该头时为 `None`,退避策略回退到指数退避。
    #[error("服务端限流{}", retry_after_secs.map(|s| format!(": 建议 {s}s 后重试")).unwrap_or_default())]
    Throttled { retry_after_secs: Option<u64> },

    /// 权限错误(HTTP 401/403)。重试无法解决,应立即终止该任务。
    #[error("权限不足(HTTP {status})")]
    Forbidden { status: u16 },

    #[error("HTTP 错误: {status} {reason}")]
    Http { status: u16, reason: String },

    #[error("URL 解析错误: {0}")]
    UrlParse(#[from] url::ParseError),

    #[error("序列化错误: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("其他错误: {0}")]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

impl From<String> for DownloadError {
    fn from(s: String) -> Self {
        DownloadError::Other(s.into())
    }
}

impl From<&str> for DownloadError {
    fn from(s: &str) -> Self {
        DownloadError::Other(s.to_string().into())
    }
}

impl DownloadError {
    pub fn network_with_source<E: std::fmt::Display>(msg: &str, source: E) -> Self {
        DownloadError::Network(format!("{msg}: {source}"))
    }

    pub fn protocol_with_source<E: std::fmt::Display>(msg: &str, source: E) -> Self {
        DownloadError::Protocol(format!("{msg}: {source}"))
    }

    /// 判断错误是否值得重试
    ///
    /// - 取消、超时、权限错误不重试
    /// - 校验失败不重试(数据已损坏)
    /// - 其他错误(网络、协议、I/O、限流等)可重试
    pub fn is_retryable(&self) -> bool {
        !matches!(
            self,
            DownloadError::Cancelled
                | DownloadError::Timeout(_)
                | DownloadError::Forbidden { .. }
                | DownloadError::ChecksumMismatch { .. }
                | DownloadError::TaskNotFound(_)
                | DownloadError::Config(_)
        )
    }
}

pub type DownloadResult<T> = Result<T, DownloadError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_error_display() {
        let err = DownloadError::Network("连接超时".into());
        assert_eq!(err.to_string(), "网络错误: 连接超时");
    }

    #[test]
    fn test_protocol_error_display() {
        let err = DownloadError::Protocol("404 Not Found".into());
        assert_eq!(err.to_string(), "协议错误: 404 Not Found");
    }

    #[test]
    fn test_io_error_from() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "文件不存在");
        let err: DownloadError = io_err.into();
        assert!(err.to_string().contains("I/O 错误"));
    }

    #[test]
    fn test_checksum_mismatch_display() {
        let err = DownloadError::ChecksumMismatch {
            expected: "abc".into(),
            actual: "def".into(),
        };
        assert!(err.to_string().contains("abc"));
        assert!(err.to_string().contains("def"));
    }

    #[test]
    fn test_cancelled_display() {
        let err = DownloadError::Cancelled;
        assert_eq!(err.to_string(), "任务已取消");
    }

    #[test]
    fn test_task_not_found_display() {
        let err = DownloadError::TaskNotFound("task-123".into());
        assert!(err.to_string().contains("task-123"));
    }

    #[test]
    fn test_connection_pool_exhausted() {
        let err = DownloadError::ConnectionPoolExhausted;
        assert_eq!(err.to_string(), "连接池已耗尽");
    }

    #[test]
    fn test_timeout_display() {
        let err = DownloadError::Timeout("30s".into());
        assert!(err.to_string().contains("30s"));
    }

    #[test]
    fn test_throttled_display_with_retry_after() {
        let err = DownloadError::Throttled {
            retry_after_secs: Some(120),
        };
        assert!(err.to_string().contains("120"));
    }

    #[test]
    fn test_throttled_display_without_retry_after() {
        let err = DownloadError::Throttled {
            retry_after_secs: None,
        };
        assert_eq!(err.to_string(), "服务端限流");
    }

    #[test]
    fn test_forbidden_display() {
        let err = DownloadError::Forbidden { status: 403 };
        assert!(err.to_string().contains("403"));
    }

    #[test]
    fn test_url_parse_error_from() {
        let err: DownloadError = url::ParseError::EmptyHost.into();
        assert!(err.to_string().contains("URL 解析错误"));
    }

    #[test]
    fn test_serde_json_error_from() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let err: DownloadError = json_err.into();
        assert!(err.to_string().contains("序列化错误"));
    }

    #[test]
    fn test_other_error() {
        let err = DownloadError::Other("未知错误".into());
        assert!(err.to_string().contains("未知错误"));
    }

    #[test]
    fn test_other_error_from_string() {
        let err: DownloadError = "简单错误".into();
        assert!(err.to_string().contains("简单错误"));
    }

    #[test]
    fn test_other_error_from_owned_string() {
        let err: DownloadError = String::from("拥有错误").into();
        assert!(err.to_string().contains("拥有错误"));
    }

    #[test]
    fn test_other_error_source_chain() {
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "管道断裂");
        let err = DownloadError::Other(Box::new(io_err));
        assert!(err.to_string().contains("管道断裂"));
    }

    #[test]
    fn test_tachyon_result_ok() {
        let result: DownloadResult<i32> = Ok(42);
        assert!(matches!(result, Ok(42)));
    }

    #[test]
    fn test_tachyon_result_err() {
        let result: DownloadResult<i32> = Err(DownloadError::Cancelled);
        assert!(result.is_err());
    }

    #[test]
    fn test_http_error_display() {
        let err = DownloadError::Http {
            status: 404,
            reason: "Not Found".into(),
        };
        assert!(err.to_string().contains("404"));
        assert!(err.to_string().contains("Not Found"));
    }

    #[test]
    fn test_network_with_source() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "连接被拒绝");
        let err = DownloadError::network_with_source("FTP 连接失败", io_err);
        assert!(matches!(err, DownloadError::Network(_)));
        assert!(err.to_string().contains("FTP 连接失败"));
        assert!(err.to_string().contains("连接被拒绝"));
    }

    #[test]
    fn test_protocol_with_source() {
        let io_err = std::io::Error::new(std::io::ErrorKind::InvalidData, "数据格式错误");
        let err = DownloadError::protocol_with_source("FTP 登录失败", io_err);
        assert!(matches!(err, DownloadError::Protocol(_)));
        assert!(err.to_string().contains("FTP 登录失败"));
        assert!(err.to_string().contains("数据格式错误"));
    }

    #[test]
    fn test_is_retryable_returns_false_for_non_retryable() {
        assert!(!DownloadError::Cancelled.is_retryable());
        assert!(!DownloadError::Timeout("30s".into()).is_retryable());
        assert!(!DownloadError::Forbidden { status: 403 }.is_retryable());
        assert!(
            !DownloadError::ChecksumMismatch {
                expected: "a".into(),
                actual: "b".into(),
            }
            .is_retryable()
        );
        assert!(!DownloadError::TaskNotFound("x".into()).is_retryable());
        assert!(!DownloadError::Config("bad".into()).is_retryable());
    }

    #[test]
    fn test_is_retryable_returns_true_for_retryable() {
        assert!(DownloadError::Network("timeout".into()).is_retryable());
        assert!(DownloadError::Protocol("bad response".into()).is_retryable());
        assert!(
            DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "reset"
            ))
            .is_retryable()
        );
        assert!(DownloadError::Fragment("short write".into()).is_retryable());
        assert!(
            DownloadError::Throttled {
                retry_after_secs: Some(5)
            }
            .is_retryable()
        );
        assert!(
            DownloadError::Throttled {
                retry_after_secs: None
            }
            .is_retryable()
        );
        assert!(
            DownloadError::Http {
                status: 500,
                reason: "Internal Server Error".into(),
            }
            .is_retryable()
        );
        assert!(DownloadError::Other("unknown".into()).is_retryable());
    }
}
