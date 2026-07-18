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

    #[error("校验失败: 已启用校验但没有期望校验摘要")]
    NoExpectedChecksum,

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
    Other(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// 服务器不支持 Range 请求(对 GET Range 返回 200 而非 206)。
    ///
    /// 不可重试:重试仍会收到 200,无意义。engine 层捕获此错误后降级为
    /// 单分片整块下载(`execute_full_download`),避免 N 片各自走 200 fallback
    /// 导致总传输量 ≈ S*N/2 的带宽浪费。
    #[error("服务器不支持 Range 请求")]
    RangeNotSupported,
}

impl serde::Serialize for DownloadError {
    /// 结构化序列化:暴露 `type`/`message`/`retryable` 三个公共字段,
    /// 以及变体特有字段(`retryAfterSecs`/`status`/`reason`/`expected`/`actual`),
    /// 供前端按错误类型分级展示(warning vs error、重试按钮等),
    /// 替代旧 `AppError::Core` 用 `to_string()` 压平丢失结构化信息的做法。
    ///
    /// `Io`/`Other` 变体含不可序列化的 `io::Error`/`Box<dyn Error>`,
    /// 用 `to_string()` 转为 `message` 字符串,其余变体保留结构化字段。
    ///
    /// 实现策略:`type_name`/`serialize_extra` 拆分为独立方法,
    /// 避免 `serialize` 主体对 17 个变体做两次 match(原 CC=33,重构后主体 CC≈5)。
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        // size_hint 传 None:变体特有字段数(0/1/2)在 type_name 之外单独决定,
        // 不值得为省几个字节预计算。JSON 序列化器内部会动态扩容。
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("type", self.type_name())?;
        map.serialize_entry("message", &self.to_string())?;
        map.serialize_entry("retryable", &self.is_retryable())?;
        self.serialize_extra::<S>(&mut map)?;
        map.end()
    }
}

impl DownloadError {
    /// 返回错误变体的类型名(前端据此分级展示)。
    fn type_name(&self) -> &'static str {
        match self {
            DownloadError::Network(_) => "Network",
            DownloadError::Protocol(_) => "Protocol",
            DownloadError::Io(_) => "Io",
            DownloadError::Fragment(_) => "Fragment",
            DownloadError::ChecksumMismatch { .. } => "ChecksumMismatch",
            DownloadError::NoExpectedChecksum => "NoExpectedChecksum",
            DownloadError::Config(_) => "Config",
            DownloadError::Cancelled => "Cancelled",
            DownloadError::TaskNotFound(_) => "TaskNotFound",
            DownloadError::ConnectionPoolExhausted => "ConnectionPoolExhausted",
            DownloadError::Timeout(_) => "Timeout",
            DownloadError::Throttled { .. } => "Throttled",
            DownloadError::Forbidden { .. } => "Forbidden",
            DownloadError::Http { .. } => "Http",
            DownloadError::UrlParse(_) => "UrlParse",
            DownloadError::Serialization(_) => "Serialization",
            DownloadError::Other(_) => "Other",
            DownloadError::RangeNotSupported => "RangeNotSupported",
        }
    }

    /// 序列化变体特有字段(仅 4 个变体有额外字段,其余 no-op)。
    /// 拆分后 `serialize` 主体不再含变体遍历,CC 从 33 降到 5。
    fn serialize_extra<S: serde::Serializer>(
        &self,
        map: &mut S::SerializeMap,
    ) -> Result<(), S::Error> {
        use serde::ser::SerializeMap;
        match self {
            DownloadError::ChecksumMismatch { expected, actual } => {
                map.serialize_entry("expected", expected)?;
                map.serialize_entry("actual", actual)?;
            }
            DownloadError::Throttled { retry_after_secs } => {
                map.serialize_entry("retryAfterSecs", retry_after_secs)?;
            }
            DownloadError::Forbidden { status } => {
                map.serialize_entry("status", status)?;
            }
            DownloadError::Http { status, reason } => {
                map.serialize_entry("status", status)?;
                map.serialize_entry("reason", reason)?;
            }
            // 其余 13 个变体无额外字段,message 已包含全部信息
            _ => {}
        }
        Ok(())
    }
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
    /// - 取消、权限错误不重试
    /// - 校验失败不重试(数据已损坏)
    /// - HTTP 4xx 客户端错误不重试(除 408/429 外,重试无法解决)
    /// - 超时、网络、协议、I/O、限流、5xx 服务端错误可重试
    pub fn is_retryable(&self) -> bool {
        match self {
            // 绝对不可重试
            DownloadError::Cancelled
            | DownloadError::Forbidden { .. }
            | DownloadError::ChecksumMismatch { .. }
            | DownloadError::NoExpectedChecksum
            | DownloadError::TaskNotFound(_)
            | DownloadError::Config(_)
            | DownloadError::UrlParse(_)
            | DownloadError::Serialization(_)
            | DownloadError::RangeNotSupported => false,

            // HTTP 4xx 客户端错误不可重试 (429/408 除外)
            DownloadError::Http { status, .. } => {
                let s = *status;
                s == 429 // Too Many Requests (限流, 等同 Throttled)
                    || s == 408 // Request Timeout (超时, 可能瞬时)
                    || s >= 500 // 5xx 服务端错误可重试
            }

            // Other 错误来源不可控(可能是配置错误等不可重试情况),默认不重试
            // 需要重试的具体错误应使用 Network/Protocol 等明确变体
            DownloadError::Other(_) => false,

            // 磁盘满不可恢复:重试只会反复失败浪费带宽,应快速失败
            DownloadError::Io(e) => e.kind() != std::io::ErrorKind::StorageFull,

            // 其余错误默认可重试
            _ => true,
        }
    }
}

pub type DownloadResult<T> = Result<T, DownloadError>;

#[cfg(test)]
mod tests {
    use std::error::Error;

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
        assert!(err.source().is_some(), "Other 变体应保留 source 链");
        assert_eq!(
            err.source().unwrap().to_string(),
            "管道断裂",
            "source 应指向原始错误"
        );
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

    // ── Serialize impl 测试:覆盖全部 17 个变体(修复 CRAP 1122 的 0% 覆盖) ──────

    /// 辅助:序列化 DownloadError 为 serde_json::Value,便于字段断言
    fn to_json(err: &DownloadError) -> serde_json::Value {
        serde_json::to_value(err).expect("DownloadError 序列化不应失败")
    }

    #[test]
    fn test_serialize_network() {
        let v = to_json(&DownloadError::Network("超时".into()));
        assert_eq!(v["type"], "Network");
        assert_eq!(v["message"], "网络错误: 超时");
        assert_eq!(v["retryable"], true);
    }

    #[test]
    fn test_serialize_protocol() {
        let v = to_json(&DownloadError::Protocol("404".into()));
        assert_eq!(v["type"], "Protocol");
        assert!(v["message"].as_str().unwrap().contains("404"));
        assert_eq!(v["retryable"], true);
    }

    #[test]
    fn test_serialize_io() {
        let err = DownloadError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "文件不存在",
        ));
        let v = to_json(&err);
        assert_eq!(v["type"], "Io");
        assert!(v["message"].as_str().unwrap().contains("文件不存在"));
        assert_eq!(v["retryable"], true); // NotFound 可重试(非 StorageFull)
    }

    #[test]
    fn test_serialize_io_storage_full_not_retryable() {
        let err = DownloadError::Io(std::io::Error::new(
            std::io::ErrorKind::StorageFull,
            "磁盘满",
        ));
        let v = to_json(&err);
        assert_eq!(v["type"], "Io");
        assert_eq!(v["retryable"], false); // StorageFull 不可重试
    }

    #[test]
    fn test_serialize_fragment() {
        let v = to_json(&DownloadError::Fragment("分片 3 失败".into()));
        assert_eq!(v["type"], "Fragment");
        assert!(v["message"].as_str().unwrap().contains("分片 3"));
        assert_eq!(v["retryable"], true);
    }

    #[test]
    fn test_serialize_checksum_mismatch_with_extra_fields() {
        let err = DownloadError::ChecksumMismatch {
            expected: "abc123".into(),
            actual: "def456".into(),
        };
        let v = to_json(&err);
        assert_eq!(v["type"], "ChecksumMismatch");
        assert_eq!(v["expected"], "abc123");
        assert_eq!(v["actual"], "def456");
        assert_eq!(v["retryable"], false);
        assert!(v["message"].as_str().unwrap().contains("abc123"));
    }

    #[test]
    fn test_serialize_no_expected_checksum() {
        let v = to_json(&DownloadError::NoExpectedChecksum);
        assert_eq!(v["type"], "NoExpectedChecksum");
        assert_eq!(v["retryable"], false);
    }

    #[test]
    fn test_serialize_config() {
        let v = to_json(&DownloadError::Config("参数错误".into()));
        assert_eq!(v["type"], "Config");
        assert_eq!(v["retryable"], false);
    }

    #[test]
    fn test_serialize_cancelled() {
        let v = to_json(&DownloadError::Cancelled);
        assert_eq!(v["type"], "Cancelled");
        assert_eq!(v["retryable"], false);
    }

    #[test]
    fn test_serialize_task_not_found() {
        let v = to_json(&DownloadError::TaskNotFound("task-42".into()));
        assert_eq!(v["type"], "TaskNotFound");
        assert_eq!(v["retryable"], false);
    }

    #[test]
    fn test_serialize_connection_pool_exhausted() {
        let v = to_json(&DownloadError::ConnectionPoolExhausted);
        assert_eq!(v["type"], "ConnectionPoolExhausted");
        assert_eq!(v["retryable"], true);
    }

    #[test]
    fn test_serialize_timeout() {
        let v = to_json(&DownloadError::Timeout("30s".into()));
        assert_eq!(v["type"], "Timeout");
        assert_eq!(v["retryable"], true);
    }

    #[test]
    fn test_serialize_throttled_with_retry_after() {
        let err = DownloadError::Throttled {
            retry_after_secs: Some(120),
        };
        let v = to_json(&err);
        assert_eq!(v["type"], "Throttled");
        assert_eq!(v["retryAfterSecs"], 120);
        assert_eq!(v["retryable"], true);
    }

    #[test]
    fn test_serialize_throttled_without_retry_after() {
        let err = DownloadError::Throttled {
            retry_after_secs: None,
        };
        let v = to_json(&err);
        assert_eq!(v["type"], "Throttled");
        assert_eq!(v["retryAfterSecs"], serde_json::Value::Null);
        assert_eq!(v["retryable"], true);
    }

    #[test]
    fn test_serialize_forbidden_with_status() {
        let err = DownloadError::Forbidden { status: 403 };
        let v = to_json(&err);
        assert_eq!(v["type"], "Forbidden");
        assert_eq!(v["status"], 403);
        assert_eq!(v["retryable"], false);
    }

    #[test]
    fn test_serialize_http_with_extra_fields() {
        let err = DownloadError::Http {
            status: 404,
            reason: "Not Found".into(),
        };
        let v = to_json(&err);
        assert_eq!(v["type"], "Http");
        assert_eq!(v["status"], 404);
        assert_eq!(v["reason"], "Not Found");
        assert_eq!(v["retryable"], false); // 404 不可重试
    }

    #[test]
    fn test_serialize_http_429_retryable() {
        let err = DownloadError::Http {
            status: 429,
            reason: "Too Many Requests".into(),
        };
        let v = to_json(&err);
        assert_eq!(v["status"], 429);
        assert_eq!(v["retryable"], true); // 429 可重试
    }

    #[test]
    fn test_serialize_http_500_retryable() {
        let err = DownloadError::Http {
            status: 503,
            reason: "Service Unavailable".into(),
        };
        let v = to_json(&err);
        assert_eq!(v["retryable"], true); // 5xx 可重试
    }

    #[test]
    fn test_serialize_url_parse() {
        let err: DownloadError = url::ParseError::EmptyHost.into();
        let v = to_json(&err);
        assert_eq!(v["type"], "UrlParse");
        assert_eq!(v["retryable"], false);
    }

    #[test]
    fn test_serialize_serialization() {
        let json_err = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let err: DownloadError = json_err.into();
        let v = to_json(&err);
        assert_eq!(v["type"], "Serialization");
        assert_eq!(v["retryable"], false);
    }

    #[test]
    fn test_serialize_other() {
        let err = DownloadError::Other("未知".into());
        let v = to_json(&err);
        assert_eq!(v["type"], "Other");
        assert_eq!(v["retryable"], false);
    }

    #[test]
    fn test_serialize_all_variants_have_three_common_fields() {
        // 确保所有变体都输出 type/message/retryable 三个公共字段
        let variants: Vec<DownloadError> = vec![
            DownloadError::Network("e".into()),
            DownloadError::Protocol("e".into()),
            DownloadError::Io(std::io::Error::other("e")),
            DownloadError::Fragment("e".into()),
            DownloadError::ChecksumMismatch {
                expected: "a".into(),
                actual: "b".into(),
            },
            DownloadError::NoExpectedChecksum,
            DownloadError::Config("e".into()),
            DownloadError::Cancelled,
            DownloadError::TaskNotFound("e".into()),
            DownloadError::ConnectionPoolExhausted,
            DownloadError::Timeout("e".into()),
            DownloadError::Throttled {
                retry_after_secs: None,
            },
            DownloadError::Forbidden { status: 403 },
            DownloadError::Http {
                status: 500,
                reason: "e".into(),
            },
            DownloadError::UrlParse(url::ParseError::EmptyHost),
            DownloadError::Serialization(
                serde_json::from_str::<serde_json::Value>("x").unwrap_err(),
            ),
            DownloadError::Other("e".into()),
        ];
        for err in &variants {
            let v = to_json(err);
            assert!(
                v.get("type").is_some(),
                "变体 {:?} 缺少 type 字段",
                err.type_name()
            );
            assert!(
                v.get("message").and_then(|m| m.as_str()).is_some(),
                "变体 {:?} 缺少 message 字段",
                err.type_name()
            );
            assert!(
                v.get("retryable").and_then(|r| r.as_bool()).is_some(),
                "变体 {:?} 缺少 retryable 字段",
                err.type_name()
            );
        }
    }

    #[test]
    fn test_is_retryable_returns_false_for_non_retryable() {
        assert!(!DownloadError::Cancelled.is_retryable());
        assert!(!DownloadError::Forbidden { status: 403 }.is_retryable());
        assert!(
            !DownloadError::ChecksumMismatch {
                expected: "a".into(),
                actual: "b".into(),
            }
            .is_retryable()
        );
        assert!(!DownloadError::NoExpectedChecksum.is_retryable());
        assert!(!DownloadError::TaskNotFound("x".into()).is_retryable());
        assert!(!DownloadError::Config("bad".into()).is_retryable());
        assert!(!DownloadError::UrlParse(url::ParseError::EmptyHost).is_retryable());
        assert!(
            !DownloadError::Serialization(
                serde_json::from_str::<serde_json::Value>("invalid").unwrap_err()
            )
            .is_retryable()
        );
    }

    #[test]
    fn test_is_retryable_returns_true_for_retryable() {
        assert!(DownloadError::Timeout("30s".into()).is_retryable());
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
        // S-5: 429/408 虽为 4xx 但仍可重试
        assert!(
            DownloadError::Http {
                status: 429,
                reason: "Too Many Requests".into(),
            }
            .is_retryable()
        );
        assert!(
            DownloadError::Http {
                status: 408,
                reason: "Request Timeout".into(),
            }
            .is_retryable()
        );
        // M-7 修复: Other 变体不再默认可重试(来源不可控,可能是配置错误)
        assert!(!DownloadError::Other("unknown".into()).is_retryable());
    }

    #[test]
    fn test_is_retryable_returns_false_for_4xx_client_errors() {
        // S-5: HTTP 4xx 客户端错误不应重试
        for status in [400, 401, 403, 404, 405, 406, 410] {
            assert!(
                !DownloadError::Http {
                    status,
                    reason: format!("Client Error {status}"),
                }
                .is_retryable(),
                "HTTP {status} 不应被重试"
            );
        }
    }

    // -----------------------------------------------------------------------
    // P1: is_retryable 完整真值表
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_retryable_truth_table_retryable_variants() {
        assert!(DownloadError::Timeout("t".into()).is_retryable());
        assert!(DownloadError::Network("n".into()).is_retryable());
        assert!(DownloadError::Protocol("p".into()).is_retryable());
        assert!(DownloadError::Io(std::io::Error::other("io")).is_retryable());
        assert!(DownloadError::Fragment("f".into()).is_retryable());
        assert!(DownloadError::ConnectionPoolExhausted.is_retryable());
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

        for status in [500, 502, 503, 504, 429, 408] {
            assert!(
                DownloadError::Http {
                    status,
                    reason: format!("R {status}")
                }
                .is_retryable(),
                "HTTP {status} 应可重试"
            );
        }
    }

    #[test]
    fn test_is_retryable_truth_table_non_retryable_variants() {
        assert!(!DownloadError::Cancelled.is_retryable());
        assert!(!DownloadError::Forbidden { status: 403 }.is_retryable());
        assert!(
            !DownloadError::ChecksumMismatch {
                expected: "a".into(),
                actual: "b".into()
            }
            .is_retryable()
        );
        assert!(!DownloadError::NoExpectedChecksum.is_retryable());
        assert!(!DownloadError::TaskNotFound("x".into()).is_retryable());
        assert!(!DownloadError::Config("c".into()).is_retryable());
        assert!(!DownloadError::UrlParse(url::ParseError::EmptyHost).is_retryable());
        assert!(
            !DownloadError::Serialization(
                serde_json::from_str::<serde_json::Value>("bad").unwrap_err()
            )
            .is_retryable()
        );
        assert!(!DownloadError::Other("o".into()).is_retryable());

        for status in [400, 401, 403, 404, 405, 406, 410] {
            assert!(
                !DownloadError::Http {
                    status,
                    reason: format!("NR {status}")
                }
                .is_retryable(),
                "HTTP {status} 不应可重试"
            );
        }
    }

    #[test]
    fn test_is_retryable_io_storage_full_not_retryable() {
        // 磁盘满不可恢复:重试只会反复失败浪费带宽
        assert!(
            !DownloadError::Io(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "磁盘空间不足"
            ))
            .is_retryable(),
            "StorageFull 应不可重试"
        );
        // 其他 Io 错误仍可重试
        assert!(
            DownloadError::Io(std::io::Error::other("io")).is_retryable(),
            "普通 Io 错误应可重试"
        );
    }
}
