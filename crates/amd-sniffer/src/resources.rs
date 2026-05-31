//! 嗅探资源管理
//!
//! 管理浏览器嗅探到的可下载资源列表,提供增删查和过滤功能。

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use amd_core::filename::extract_filename_from_url;
use serde::{Deserialize, Serialize};

use crate::capture::{CaptureConfig, identify_resource, should_capture};

/// 嗅探到的资源
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnifferResource {
    /// 唯一标识
    pub id: String,
    /// 资源 URL
    pub url: String,
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

/// 资源管理器
///
/// 使用 `RwLock` 实现读写分离:读取操作(如列出资源)不阻塞写入操作。
/// `std::sync::RwLock` 不会因 panic 而毒化锁。
pub struct ResourceManager {
    resources: RwLock<HashMap<String, SnifferResource>>,
    config: CaptureConfig,
}

impl ResourceManager {
    /// 创建新的资源管理器
    pub fn new(config: CaptureConfig) -> Self {
        Self {
            resources: RwLock::new(HashMap::new()),
            config,
        }
    }

    /// 处理一个拦截到的请求 URL
    ///
    /// 如果匹配捕获规则且尚未记录,则添加到资源列表。
    /// 返回是否为新发现的资源。
    pub fn on_request(
        &self,
        url: &str,
        content_type: Option<&str>,
        file_size: Option<u64>,
        source_page: Option<String>,
    ) -> bool {
        if !should_capture(url, &self.config) {
            return false;
        }

        let resource_type = identify_resource(url);
        tracing::debug!(url, resource_type = ?resource_type, "嗅探捕获资源");
        let file_name = extract_filename_from_url(url);

        // 检查最小文件大小
        if let Some(size) = file_size
            && size < self.config.min_size
        {
            return false;
        }

        let id = generate_id(url);
        let mut resources = self.resources.write().expect("RwLock 不应被毒化");

        if resources.contains_key(&id) {
            return false;
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        resources.insert(
            id.clone(),
            SnifferResource {
                id,
                url: url.to_string(),
                file_name,
                resource_type: resource_type.as_str().to_string(),
                file_size,
                content_type: content_type.map(|s| s.to_string()),
                discovered_at: now,
                source_page,
            },
        );

        true
    }

    /// 获取所有已发现的资源
    pub fn get_all(&self) -> Vec<SnifferResource> {
        let resources = self.resources.read().expect("RwLock 不应被毒化");
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

    /// 移除资源
    pub fn remove(&self, id: &str) -> bool {
        let mut resources = self.resources.write().expect("RwLock 不应被毒化");
        resources.remove(id).is_some()
    }

    /// 清空所有资源
    pub fn clear(&self) {
        let mut resources = self.resources.write().expect("RwLock 不应被毒化");
        resources.clear();
    }

    /// 资源数量
    pub fn count(&self) -> usize {
        let resources = self.resources.read().expect("RwLock 不应被毒化");
        resources.len()
    }

    /// 更新捕获配置
    pub fn set_config(&mut self, config: CaptureConfig) {
        self.config = config;
    }

    /// 获取当前配置的不可变引用
    pub fn config(&self) -> &CaptureConfig {
        &self.config
    }
}

impl Default for ResourceManager {
    fn default() -> Self {
        Self::new(CaptureConfig::default())
    }
}

/// 生成资源唯一 ID
fn generate_id(url: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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

    #[test]
    fn test_get_all_sorted_by_time() {
        let rm = ResourceManager::default();
        rm.on_request("http://example.com/a.mp4", None, Some(10240), None);
        rm.on_request("http://example.com/b.mp3", None, Some(10240), None);
        let list = rm.get_all();
        assert_eq!(list.len(), 2);
        // 最新的在前
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
}
