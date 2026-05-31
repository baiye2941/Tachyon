//! 统一错误类型

use thiserror::Error;

/// AI Model Downloader 全局错误类型
#[derive(Error, Debug)]
pub enum AmdError {
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

impl From<String> for AmdError {
    fn from(s: String) -> Self {
        AmdError::Other(s.into())
    }
}

impl From<&str> for AmdError {
    fn from(s: &str) -> Self {
        AmdError::Other(s.to_string().into())
    }
}

impl AmdError {
    pub fn network_with_source<E: std::fmt::Display>(msg: &str, source: E) -> Self {
        AmdError::Network(format!("{msg}: {source}"))
    }

    pub fn protocol_with_source<E: std::fmt::Display>(msg: &str, source: E) -> Self {
        AmdError::Protocol(format!("{msg}: {source}"))
    }
}

pub type AmdResult<T> = Result<T, AmdError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_error_display() {
        let err = AmdError::Network("连接超时".into());
        assert_eq!(err.to_string(), "网络错误: 连接超时");
    }

    #[test]
    fn test_protocol_error_display() {
        let err = AmdError::Protocol("404 Not Found".into());
        assert_eq!(err.to_string(), "协议错误: 404 Not Found");
    }

    #[test]
    fn test_io_error_from() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "文件不存在");
        let err: AmdError = io_err.into();
        assert!(err.to_string().contains("I/O 错误"));
    }

    #[test]
    fn test_checksum_mismatch_display() {
        let err = AmdError::ChecksumMismatch {
            expected: "abc".into(),
            actual: "def".into(),
        };
        assert!(err.to_string().contains("abc"));
        assert!(err.to_string().contains("def"));
    }

    #[test]
    fn test_cancelled_display() {
        let err = AmdError::Cancelled;
        assert_eq!(err.to_string(), "任务已取消");
    }

    #[test]
    fn test_task_not_found_display() {
        let err = AmdError::TaskNotFound("task-123".into());
        assert!(err.to_string().contains("task-123"));
    }

    #[test]
    fn test_connection_pool_exhausted() {
        let err = AmdError::ConnectionPoolExhausted;
        assert_eq!(err.to_string(), "连接池已耗尽");
    }

    #[test]
    fn test_timeout_display() {
        let err = AmdError::Timeout("30s".into());
        assert!(err.to_string().contains("30s"));
    }

    #[test]
    fn test_throttled_display_with_retry_after() {
        let err = AmdError::Throttled {
            retry_after_secs: Some(120),
        };
        assert!(err.to_string().contains("120"));
    }

    #[test]
    fn test_throttled_display_without_retry_after() {
        let err = AmdError::Throttled {
            retry_after_secs: None,
        };
        assert_eq!(err.to_string(), "服务端限流");
    }

    #[test]
    fn test_forbidden_display() {
        let err = AmdError::Forbidden { status: 403 };
        assert!(err.to_string().contains("403"));
    }

    #[test]
    fn test_url_parse_error_from() {
        let err: AmdError = url::ParseError::EmptyHost.into();
        assert!(err.to_string().contains("URL 解析错误"));
    }

    #[test]
    fn test_serde_json_error_from() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let err: AmdError = json_err.into();
        assert!(err.to_string().contains("序列化错误"));
    }

    #[test]
    fn test_other_error() {
        let err = AmdError::Other("未知错误".into());
        assert!(err.to_string().contains("未知错误"));
    }

    #[test]
    fn test_other_error_from_string() {
        let err: AmdError = "简单错误".into();
        assert!(err.to_string().contains("简单错误"));
    }

    #[test]
    fn test_other_error_from_owned_string() {
        let err: AmdError = String::from("拥有错误").into();
        assert!(err.to_string().contains("拥有错误"));
    }

    #[test]
    fn test_other_error_source_chain() {
        let io_err = std::io::Error::new(std::io::ErrorKind::BrokenPipe, "管道断裂");
        let err = AmdError::Other(Box::new(io_err));
        assert!(err.to_string().contains("管道断裂"));
    }

    #[test]
    fn test_amd_result_ok() {
        let result: AmdResult<i32> = Ok(42);
        assert!(matches!(result, Ok(42)));
    }

    #[test]
    fn test_amd_result_err() {
        let result: AmdResult<i32> = Err(AmdError::Cancelled);
        assert!(result.is_err());
    }

    #[test]
    fn test_http_error_display() {
        let err = AmdError::Http {
            status: 404,
            reason: "Not Found".into(),
        };
        assert!(err.to_string().contains("404"));
        assert!(err.to_string().contains("Not Found"));
    }

    #[test]
    fn test_network_with_source() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "连接被拒绝");
        let err = AmdError::network_with_source("FTP 连接失败", io_err);
        assert!(matches!(err, AmdError::Network(_)));
        assert!(err.to_string().contains("FTP 连接失败"));
        assert!(err.to_string().contains("连接被拒绝"));
    }

    #[test]
    fn test_protocol_with_source() {
        let io_err = std::io::Error::new(std::io::ErrorKind::InvalidData, "数据格式错误");
        let err = AmdError::protocol_with_source("FTP 登录失败", io_err);
        assert!(matches!(err, AmdError::Protocol(_)));
        assert!(err.to_string().contains("FTP 登录失败"));
        assert!(err.to_string().contains("数据格式错误"));
    }
}
