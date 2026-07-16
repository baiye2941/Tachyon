//! 嗅探资源管理
//!
//! 管理上层 adapter 注入的可下载资源列表,提供增删查、去重、
//! 敏感参数脱敏等功能。资源来源(浏览器扩展 / CDP / WebView 注入 /
//! 手动添加)由调用方决定,本模块不关心来源。
//!
//! 审计 A-07:`ResourceManager` 是捕获规则与资源集合的**唯一 owner**;
//! app 层 `SnifferService` 仅作 async/Tauri 适配,不得再持有第二份存储。

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tachyon_core::safety::extract_filename_from_url;

use crate::capture::{CaptureConfig, identify_resource, should_capture};

/// 敏感查询参数名称列表
const SENSITIVE_PARAM_NAMES: &[&str] = &[
    "token",
    "key",
    "secret",
    "auth",
    "session",
    "password",
    "passwd",
    "credential",
    "access_token",
    "api_key",
    "apikey",
    "jwt",
    "bearer",
    "sig",
    "signature",
    "client_secret",
    "refresh_token",
];

/// 单条 URL 过滤规则最大长度
pub const MAX_FILTER_LENGTH: usize = 256;
/// URL 过滤规则数量上限
pub const MAX_FILTER_COUNT: usize = 100;
/// 资源列表容量上限(超限淘汰最旧)
pub const MAX_SNIFFER_RESOURCES: usize = 1000;

/// 嗅探到的资源
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnifferResource {
    /// 唯一标识
    pub id: String,
    /// 资源 URL(脱敏后,用于显示和去重)
    pub url: String,
    /// 资源原始 URL(含凭据,仅后端建任务使用,不序列化到 IPC/事件)
    #[serde(skip_serializing, default)]
    pub download_url: String,
    /// 文件名
    #[serde(rename = "name")]
    pub file_name: String,
    /// 资源类型
    #[serde(rename = "type")]
    pub resource_type: String,
    /// 文件大小(字节,如已知)
    #[serde(rename = "size")]
    pub file_size: Option<u64>,
    /// Content-Type
    pub content_type: Option<String>,
    /// 发现时间(Unix 时间戳)
    pub discovered_at: u64,
    /// 来源页面 URL
    pub source_page: Option<String>,
}

/// 捕获配置校验错误(A-07:规则集中在 sniffer crate)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureConfigError(pub String);

impl std::fmt::Display for CaptureConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CaptureConfigError {}

/// 校验 `CaptureConfig` 过滤器约束(与历史 app 层规则对齐)
pub fn validate_capture_config(config: &CaptureConfig) -> Result<(), CaptureConfigError> {
    for f in &config.url_filters {
        if f.is_empty() {
            return Err(CaptureConfigError("过滤规则不能为空".into()));
        }
        if f.len() > MAX_FILTER_LENGTH {
            return Err(CaptureConfigError(format!(
                "过滤规则长度不能超过 {MAX_FILTER_LENGTH} 字符"
            )));
        }
    }
    if config.url_filters.len() > MAX_FILTER_COUNT {
        return Err(CaptureConfigError(format!(
            "过滤规则数量已达上限 {MAX_FILTER_COUNT}"
        )));
    }
    let mut seen = std::collections::HashSet::with_capacity(config.url_filters.len());
    for f in &config.url_filters {
        if !seen.insert(f.as_str()) {
            return Err(CaptureConfigError("过滤规则已存在".into()));
        }
    }
    if config.min_size > i64::MAX as u64 {
        return Err(CaptureConfigError("最小文件大小值非法".into()));
    }
    Ok(())
}

/// 资源管理器(唯一 owner)
///
/// `config` 与 `resources` 均在 `RwLock` 内,可在 `Arc` 上安全更新。
pub struct ResourceManager {
    resources: RwLock<HashMap<String, SnifferResource>>,
    config: RwLock<CaptureConfig>,
}

impl ResourceManager {
    /// 创建新的资源管理器
    pub fn new(config: CaptureConfig) -> Self {
        Self {
            resources: RwLock::new(HashMap::new()),
            config: RwLock::new(config),
        }
    }

    /// 处理拦截请求;成功新增时返回资源快照
    ///
    /// - `file_size=None`(手动 URL 等)跳过 min_size
    /// - `file_size=Some` 且 `< min_size` 则拒绝
    pub fn capture(
        &self,
        url: &str,
        content_type: Option<&str>,
        file_size: Option<u64>,
        source_page: Option<String>,
    ) -> Option<SnifferResource> {
        let config = self.config.read().unwrap_or_else(|e| {
            tracing::warn!("CaptureConfig RwLock 中毒,恢复使用内部数据");
            e.into_inner()
        });
        if !should_capture(url, &config) {
            return None;
        }
        if let Some(size) = file_size
            && size < config.min_size
        {
            return None;
        }
        drop(config);

        let resource_type = identify_resource(url);
        tracing::debug!(url, resource_type = ?resource_type, "嗅探捕获资源");
        let file_name = extract_filename_from_url(url);
        let id = generate_id(url);
        let redacted_url = redact_sensitive_params(url);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let resource = SnifferResource {
            id: id.clone(),
            url: redacted_url,
            download_url: url.to_string(),
            file_name,
            resource_type: resource_type.as_str().to_string(),
            file_size,
            content_type: content_type.map(|s| s.to_string()),
            discovered_at: now,
            source_page,
        };

        let mut resources = self.resources.write().unwrap_or_else(|e| {
            tracing::warn!("resources RwLock 中毒,恢复使用内部数据");
            e.into_inner()
        });

        if resources.contains_key(&id) {
            return None;
        }

        // 容量上限:淘汰 discovered_at 最旧的一条
        if resources.len() >= MAX_SNIFFER_RESOURCES
            && let Some(oldest_id) = resources
                .values()
                .min_by_key(|r| r.discovered_at)
                .map(|r| r.id.clone())
        {
            resources.remove(&oldest_id);
        }

        resources.insert(id, resource.clone());
        Some(resource)
    }

    /// 兼容旧 API:返回是否为新资源
    pub fn on_request(
        &self,
        url: &str,
        content_type: Option<&str>,
        file_size: Option<u64>,
        source_page: Option<String>,
    ) -> bool {
        self.capture(url, content_type, file_size, source_page)
            .is_some()
    }

    /// 仅 URL 的手动添加入口(无 size → 不应用 min_size)
    pub fn add_url(&self, url: &str) -> Option<SnifferResource> {
        self.capture(url, None, None, None)
    }

    /// 获取所有已发现的资源(按发现时间倒序)
    pub fn get_all(&self) -> Vec<SnifferResource> {
        let resources = self.resources.read().unwrap_or_else(|e| {
            tracing::warn!("resources RwLock 中毒,恢复使用内部数据");
            e.into_inner()
        });
        let mut list: Vec<_> = resources.values().cloned().collect();
        list.sort_by_key(|r| std::cmp::Reverse(r.discovered_at));
        list
    }

    /// 按类型过滤资源
    pub fn get_by_type(&self, resource_type: &str) -> Vec<SnifferResource> {
        self.get_all()
            .into_iter()
            .filter(|r| r.resource_type == resource_type)
            .collect()
    }

    /// 按 id 取资源(含 download_url,仅后端内部使用)
    pub fn get_by_id(&self, id: &str) -> Option<SnifferResource> {
        let resources = self.resources.read().unwrap_or_else(|e| {
            tracing::warn!("resources RwLock 中毒,恢复使用内部数据");
            e.into_inner()
        });
        resources.get(id).cloned()
    }

    /// 移除资源
    pub fn remove(&self, id: &str) -> bool {
        let mut resources = self.resources.write().unwrap_or_else(|e| {
            tracing::warn!("resources RwLock 中毒,恢复使用内部数据");
            e.into_inner()
        });
        resources.remove(id).is_some()
    }

    /// 清空所有资源
    pub fn clear(&self) {
        let mut resources = self.resources.write().unwrap_or_else(|e| {
            tracing::warn!("resources RwLock 中毒,恢复使用内部数据");
            e.into_inner()
        });
        resources.clear();
    }

    /// 资源数量
    pub fn count(&self) -> usize {
        let resources = self.resources.read().unwrap_or_else(|e| {
            tracing::warn!("resources RwLock 中毒,恢复使用内部数据");
            e.into_inner()
        });
        resources.len()
    }

    /// 更新捕获配置(经校验)
    pub fn set_config(&self, config: CaptureConfig) -> Result<(), CaptureConfigError> {
        validate_capture_config(&config)?;
        let mut guard = self.config.write().unwrap_or_else(|e| {
            tracing::warn!("CaptureConfig RwLock 中毒,恢复使用内部数据");
            e.into_inner()
        });
        *guard = config;
        Ok(())
    }

    /// 获取当前配置克隆
    pub fn config(&self) -> CaptureConfig {
        self.config
            .read()
            .unwrap_or_else(|e| {
                tracing::warn!("CaptureConfig RwLock 中毒,恢复使用内部数据");
                e.into_inner()
            })
            .clone()
    }

    /// 追加一条 URL 过滤规则
    pub fn add_filter(&self, filter: String) -> Result<(), CaptureConfigError> {
        if filter.is_empty() {
            return Err(CaptureConfigError("过滤规则不能为空".into()));
        }
        if filter.len() > MAX_FILTER_LENGTH {
            return Err(CaptureConfigError(format!(
                "过滤规则长度不能超过 {MAX_FILTER_LENGTH} 字符"
            )));
        }
        let mut config = self.config.write().unwrap_or_else(|e| {
            tracing::warn!("CaptureConfig RwLock 中毒,恢复使用内部数据");
            e.into_inner()
        });
        if config.url_filters.len() >= MAX_FILTER_COUNT {
            return Err(CaptureConfigError(format!(
                "过滤规则数量已达上限 {MAX_FILTER_COUNT}"
            )));
        }
        if config.url_filters.contains(&filter) {
            return Err(CaptureConfigError("过滤规则已存在".into()));
        }
        tracing::info!(filter = %filter, "添加嗅探过滤规则");
        config.url_filters.push(filter);
        Ok(())
    }
}

impl Default for ResourceManager {
    fn default() -> Self {
        Self::new(CaptureConfig::default())
    }
}

/// 生成资源唯一 ID(稳定 hash,同 URL 同 id)
fn generate_id(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// 脱敏 URL 中的敏感查询参数
pub fn redact_sensitive_params(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.to_string();
    };

    let has_sensitive = parsed.query_pairs().any(|(key, _)| {
        let lower = key.to_ascii_lowercase();
        SENSITIVE_PARAM_NAMES.iter().any(|&s| lower.contains(s))
    });

    if !has_sensitive {
        return url.to_string();
    }

    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(key, value)| {
            let lower = key.to_ascii_lowercase();
            if SENSITIVE_PARAM_NAMES.iter().any(|&s| lower.contains(s)) {
                (key.into_owned(), "[REDACTED]".to_string())
            } else {
                (key.into_owned(), value.into_owned())
            }
        })
        .collect();

    parsed.set_query(None);
    for (key, value) in &pairs {
        parsed.query_pairs_mut().append_pair(key, value);
    }

    parsed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resource_manager_default() {
        let rm = ResourceManager::default();
        assert_eq!(rm.count(), 0);
    }

    #[test]
    fn test_on_request_captures_video() {
        let rm = ResourceManager::default();
        let is_new = rm.on_request(
            "http://example.com/video.mp4",
            Some("video/mp4"),
            Some(10 * 1024 * 1024),
            None,
        );
        assert!(is_new);
        assert_eq!(rm.count(), 1);
    }

    #[test]
    fn test_capture_returns_resource() {
        let rm = ResourceManager::default();
        let res = rm
            .capture(
                "http://example.com/video.mp4",
                Some("video/mp4"),
                Some(10 * 1024 * 1024),
                None,
            )
            .expect("应捕获");
        assert_eq!(res.resource_type, "video");
        assert_eq!(res.file_name, "video.mp4");
        assert!(res.download_url.contains("video.mp4"));
    }

    #[test]
    fn test_on_request_ignores_html() {
        let rm = ResourceManager::default();
        let is_new = rm.on_request(
            "http://example.com/page.html",
            Some("text/html"),
            None,
            None,
        );
        assert!(!is_new);
        assert_eq!(rm.count(), 0);
    }

    #[test]
    fn test_on_request_dedup() {
        let rm = ResourceManager::default();
        assert!(rm.on_request("http://example.com/file.zip", None, Some(2048), None));
        assert!(!rm.on_request("http://example.com/file.zip", None, Some(2048), None));
        assert_eq!(rm.count(), 1);
    }

    #[test]
    fn test_on_request_min_size_filter() {
        let rm = ResourceManager::default();
        // 默认 min_size = 1024
        assert!(!rm.on_request("http://example.com/tiny.zip", None, Some(100), None));
        assert!(rm.on_request("http://example.com/big.zip", None, Some(2048), None));
    }

    /// A-07:无 size 的手动 URL 不因 min_size 被拒
    #[test]
    fn test_add_url_skips_min_size_when_unknown() {
        let rm = ResourceManager::default();
        let res = rm.add_url("http://example.com/maybe-tiny.zip");
        assert!(
            res.is_some(),
            "未知大小时不应应用 min_size,否则手动添加永远失败"
        );
    }

    #[test]
    fn test_get_all_sorted_by_time() {
        let rm = ResourceManager::default();
        rm.on_request("http://example.com/a.mp4", None, Some(10240), None);
        rm.on_request("http://example.com/b.mp3", None, Some(10240), None);
        let list = rm.get_all();
        assert_eq!(list.len(), 2);
        assert!(list[0].discovered_at >= list[1].discovered_at);
    }

    #[test]
    fn test_get_by_type() {
        let rm = ResourceManager::default();
        rm.on_request("http://example.com/a.mp4", None, Some(10240), None);
        rm.on_request("http://example.com/b.mp3", None, Some(10240), None);
        rm.on_request("http://example.com/c.zip", None, Some(10240), None);
        let videos = rm.get_by_type("video");
        assert_eq!(videos.len(), 1);
        let archives = rm.get_by_type("archive");
        assert_eq!(archives.len(), 1);
    }

    #[test]
    fn test_remove() {
        let rm = ResourceManager::default();
        rm.on_request("http://example.com/file.zip", None, Some(2048), None);
        let list = rm.get_all();
        let id = &list[0].id;
        assert!(rm.remove(id));
        assert_eq!(rm.count(), 0);
        assert!(!rm.remove("nonexistent"));
    }

    #[test]
    fn test_clear() {
        let rm = ResourceManager::default();
        rm.on_request("http://example.com/a.mp4", None, Some(10240), None);
        rm.on_request("http://example.com/b.mp3", None, Some(10240), None);
        rm.clear();
        assert_eq!(rm.count(), 0);
    }

    #[test]
    fn test_get_by_id() {
        let rm = ResourceManager::default();
        rm.on_request("http://example.com/file.bin", None, Some(2048), None);
        let id = rm.get_all()[0].id.clone();
        let got = rm.get_by_id(&id).expect("应按 id 取回");
        assert_eq!(got.id, id);
        assert!(got.download_url.contains("file.bin"));
    }

    #[test]
    fn test_download_url_not_serialized_to_ipc() {
        let r = SnifferResource {
            id: "id1".into(),
            url: "https://cdn.example.com/file.bin".into(),
            download_url: "https://cdn.example.com/file.bin?token=secret".into(),
            file_name: "file.bin".into(),
            resource_type: "other".into(),
            file_size: Some(1),
            content_type: None,
            discovered_at: 0,
            source_page: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(
            !json.contains("token=secret"),
            "完整 URL 不得序列化: {json}"
        );
        assert!(
            !json.contains("downloadUrl"),
            "downloadUrl 字段不得出现: {json}"
        );
        assert!(json.contains("cdn.example.com"));
    }

    #[test]
    fn test_validate_and_set_config_rejects_empty_filter() {
        let rm = ResourceManager::default();
        let mut cfg = rm.config();
        cfg.url_filters.push(String::new());
        let err = rm.set_config(cfg).unwrap_err();
        assert!(err.to_string().contains("不能为空"));
    }

    #[test]
    fn test_add_filter_duplicate() {
        let rm = ResourceManager::default();
        rm.add_filter("cdn.example.com".into()).unwrap();
        let err = rm.add_filter("cdn.example.com".into()).unwrap_err();
        assert!(err.to_string().contains("已存在"));
    }

    #[test]
    fn test_add_filter_then_should_capture() {
        let rm = ResourceManager::default();
        rm.add_filter("cdn.example.com".into()).unwrap();
        assert!(rm.add_url("http://other.com/video.mp4").is_none());
        assert!(rm.add_url("http://cdn.example.com/video.mp4").is_some());
    }
}
