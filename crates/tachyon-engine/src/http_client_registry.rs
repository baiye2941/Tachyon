//! 共享 HttpClient 注册表(审计 HTTP-15)
//!
//! `ConnectionPool` 仅是并发信号量,真正的 TCP/TLS/H2 复用由 `reqwest::Client` 负责。
//! 本注册表按连接身份键缓存 `HttpClient`,使跨任务/跨镜像在同一身份下共享底层连接池。
//!
//! 键包含:UA、proxy、timeouts、http2/quic、pool 参数、headers 内容。
//! 身份变化时创建新 client,不污染旧池。

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use dashmap::DashMap;
use tachyon_core::DownloadResult;
use tachyon_core::config::ConnectionConfig;
use tachyon_protocol::HttpClient;

/// 连接身份键(决定能否共享同一 reqwest Client)
#[derive(Debug, Clone, Eq)]
struct ClientIdentity {
    user_agent: String,
    proxy: Option<String>,
    connect_secs: u64,
    read_secs: u64,
    enable_http2: bool,
    enable_quic: bool,
    pool_max_idle_per_host: usize,
    keep_alive_secs: u64,
    /// 排序后的 header 对,保证 Hash 稳定
    headers: Vec<(String, String)>,
    /// auth_bearer 指纹(不存明文 token)
    auth_token_fp: u64,
}

impl PartialEq for ClientIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.user_agent == other.user_agent
            && self.proxy == other.proxy
            && self.connect_secs == other.connect_secs
            && self.read_secs == other.read_secs
            && self.enable_http2 == other.enable_http2
            && self.enable_quic == other.enable_quic
            && self.pool_max_idle_per_host == other.pool_max_idle_per_host
            && self.keep_alive_secs == other.keep_alive_secs
            && self.headers == other.headers
            && self.auth_token_fp == other.auth_token_fp
    }
}

impl Hash for ClientIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.user_agent.hash(state);
        self.proxy.hash(state);
        self.connect_secs.hash(state);
        self.read_secs.hash(state);
        self.enable_http2.hash(state);
        self.enable_quic.hash(state);
        self.pool_max_idle_per_host.hash(state);
        self.keep_alive_secs.hash(state);
        self.headers.hash(state);
        self.auth_token_fp.hash(state);
    }
}

impl ClientIdentity {
    fn from_parts(
        user_agent: &str,
        proxy: Option<&str>,
        connect_secs: u64,
        read_secs: u64,
        conn: Option<&ConnectionConfig>,
        headers: &HashMap<String, String>,
        auth_bearer: Option<&str>,
    ) -> Self {
        let (enable_http2, enable_quic, pool_max_idle_per_host, keep_alive_secs) =
            if let Some(c) = conn {
                (
                    c.enable_http2,
                    c.enable_quic,
                    c.max_connections_per_host as usize,
                    c.keep_alive_timeout_secs,
                )
            } else {
                (false, false, 16, 30)
            };
        let mut headers: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
            .collect();
        headers.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let auth_token_fp = auth_bearer
            .map(|t| {
                use std::collections::hash_map::DefaultHasher;
                let mut h = DefaultHasher::new();
                t.hash(&mut h);
                h.finish()
            })
            .unwrap_or(0);
        Self {
            user_agent: if user_agent.is_empty() {
                tachyon_core::config::USER_AGENT.to_string()
            } else {
                user_agent.to_string()
            },
            proxy: proxy.map(|s| s.to_string()),
            connect_secs,
            read_secs,
            enable_http2,
            enable_quic,
            pool_max_idle_per_host,
            keep_alive_secs,
            headers,
            auth_token_fp,
        }
    }
}

/// 进程内共享 HttpClient 注册表
#[derive(Default)]
pub struct HttpClientRegistry {
    map: DashMap<ClientIdentity, Arc<HttpClient>>,
}

impl HttpClientRegistry {
    pub fn new() -> Self {
        Self {
            map: DashMap::new(),
        }
    }

    /// 获取或创建匹配身份的 HttpClient(Arc 共享)
    #[allow(clippy::too_many_arguments)]
    pub fn get_or_create(
        &self,
        user_agent: &str,
        proxy: Option<&str>,
        connect_secs: u64,
        read_secs: u64,
        conn: Option<&ConnectionConfig>,
        headers: &HashMap<String, String>,
        auth_bearer: Option<&str>,
    ) -> DownloadResult<Arc<HttpClient>> {
        let id = ClientIdentity::from_parts(
            user_agent,
            proxy,
            connect_secs,
            read_secs,
            conn,
            headers,
            auth_bearer,
        );
        if let Some(existing) = self.map.get(&id) {
            return Ok(Arc::clone(existing.value()));
        }
        // 双检:entry API 避免并发重复 build
        use dashmap::mapref::entry::Entry;
        match self.map.entry(id.clone()) {
            Entry::Occupied(o) => Ok(Arc::clone(o.get())),
            Entry::Vacant(v) => {
                let client = if let Some(c) = conn {
                    HttpClient::with_connection_config_and_headers(
                        c,
                        connect_secs,
                        read_secs,
                        proxy,
                        user_agent,
                        headers,
                    )?
                } else {
                    HttpClient::with_timeouts_and_headers(
                        connect_secs,
                        read_secs,
                        proxy,
                        user_agent,
                        headers,
                    )?
                };
                let client = client.with_auth_bearer(auth_bearer.map(str::to_owned));
                let arc = Arc::new(client);
                v.insert(Arc::clone(&arc));
                Ok(arc)
            }
        }
    }

    /// 当前缓存条目数(测试/诊断)
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// 全局默认注册表
pub fn global_http_client_registry() -> &'static HttpClientRegistry {
    use std::sync::OnceLock;
    static REG: OnceLock<HttpClientRegistry> = OnceLock::new();
    REG.get_or_init(HttpClientRegistry::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_registry_reuses_same_identity() {
        let reg = HttpClientRegistry::new();
        let headers = HashMap::new();
        let a = reg
            .get_or_create("UA-1", None, 5, 10, None, &headers, None)
            .unwrap();
        let b = reg
            .get_or_create("UA-1", None, 5, 10, None, &headers, None)
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b), "同身份应返回同一 Arc");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn test_registry_separates_different_ua() {
        let reg = HttpClientRegistry::new();
        let headers = HashMap::new();
        let a = reg
            .get_or_create("UA-A", None, 5, 10, None, &headers, None)
            .unwrap();
        let b = reg
            .get_or_create("UA-B", None, 5, 10, None, &headers, None)
            .unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn test_registry_separates_proxy() {
        let reg = HttpClientRegistry::new();
        let headers = HashMap::new();
        let a = reg
            .get_or_create(
                "UA",
                Some("http://127.0.0.1:1"),
                5,
                10,
                None,
                &headers,
                None,
            )
            .unwrap();
        let b = reg
            .get_or_create(
                "UA",
                Some("http://127.0.0.1:2"),
                5,
                10,
                None,
                &headers,
                None,
            )
            .unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn test_registry_separates_auth_bearer() {
        let reg = HttpClientRegistry::new();
        let headers = HashMap::new();
        let a = reg
            .get_or_create("UA", None, 5, 10, None, &headers, None)
            .unwrap();
        let b = reg
            .get_or_create("UA", None, 5, 10, None, &headers, Some("tok-a"))
            .unwrap();
        let c = reg
            .get_or_create("UA", None, 5, 10, None, &headers, Some("tok-b"))
            .unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
        assert!(!Arc::ptr_eq(&b, &c));
        assert!(b.has_auth_bearer());
        assert!(!a.has_auth_bearer());
    }

    /// 空 UA 应回退到全局默认 USER_AGENT,避免下游用空 UA 请求。
    /// 覆盖 from_parts 的 `user_agent.is_empty()` 分支。
    #[test]
    fn test_registry_empty_ua_falls_back_to_default_user_agent() {
        let reg = HttpClientRegistry::new();
        let headers = HashMap::new();
        let a = reg
            .get_or_create("", None, 5, 10, None, &headers, None)
            .unwrap();
        // 再次用空 UA,应复用同一 client(身份键用默认 UA)
        let b = reg
            .get_or_create("", None, 5, 10, None, &headers, None)
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b), "空 UA 应映射到默认 UA,复用 client");
        assert_eq!(reg.len(), 1);
    }

    /// conn=Some 时走 with_connection_config_and_headers 路径。
    /// 覆盖 from_parts 的 conn=Some 分支 + get_or_create 的 conn=Some 构建分支。
    #[test]
    fn test_registry_with_connection_config_builds_client() {
        use tachyon_core::config::ConnectionConfig;
        let reg = HttpClientRegistry::new();
        let headers = HashMap::new();
        let conn = ConnectionConfig {
            enable_http2: true,
            enable_quic: false,
            max_connections_per_host: 8,
            max_global_connections: 256,
            keep_alive_timeout_secs: 90,
            connect_timeout_secs: 10,
        };
        let a = reg
            .get_or_create("UA-Conn", None, 5, 10, Some(&conn), &headers, None)
            .unwrap();
        // 相同 conn 应复用
        let b = reg
            .get_or_create("UA-Conn", None, 5, 10, Some(&conn), &headers, None)
            .unwrap();
        assert!(Arc::ptr_eq(&a, &b), "同 conn 身份应复用 client");
        // 不同 conn 参数(pool_max_idle_per_host 不同)应分离
        let conn2 = ConnectionConfig {
            enable_http2: true,
            enable_quic: false,
            max_connections_per_host: 16,
            max_global_connections: 256,
            keep_alive_timeout_secs: 90,
            connect_timeout_secs: 10,
        };
        let c = reg
            .get_or_create("UA-Conn", None, 5, 10, Some(&conn2), &headers, None)
            .unwrap();
        assert!(!Arc::ptr_eq(&a, &c), "pool_max_idle_per_host 不同应分离");
    }

    /// headers 不同应分离(覆盖 headers 排序 + hash 路径)。
    #[test]
    fn test_registry_separates_different_headers() {
        let reg = HttpClientRegistry::new();
        let mut h1 = HashMap::new();
        h1.insert("X-Custom".to_string(), "v1".to_string());
        let mut h2 = HashMap::new();
        h2.insert("X-Custom".to_string(), "v2".to_string());
        let a = reg
            .get_or_create("UA", None, 5, 10, None, &h1, None)
            .unwrap();
        let b = reg
            .get_or_create("UA", None, 5, 10, None, &h2, None)
            .unwrap();
        assert!(!Arc::ptr_eq(&a, &b), "headers 不同应分离 client");
    }

    /// is_empty 应在空注册表返回 true,有条目后返回 false。
    /// 覆盖 is_empty 分支。
    #[test]
    fn test_registry_is_empty_and_len() {
        let reg = HttpClientRegistry::new();
        assert!(reg.is_empty(), "新注册表应 is_empty");
        assert_eq!(reg.len(), 0);
        let headers = HashMap::new();
        let _ = reg
            .get_or_create("UA", None, 5, 10, None, &headers, None)
            .unwrap();
        assert!(!reg.is_empty(), "有条目后应非空");
        assert_eq!(reg.len(), 1);
    }
}
