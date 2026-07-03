//! 嗅探器应用服务
//!
//! 封装嗅探器相关的业务规则，从 AppState 和 Tauri command 层提取的纯逻辑层。
//! 不直接依赖 Tauri 框架，可被 CLI/daemon/headless API 复用。
//!
//! SnifferService 封装资源和过滤器的存储与校验逻辑，
//! AppState 仅持有 `Arc<SnifferService>` 而非直接暴露 sniffer 字段。

use std::sync::Arc;

use tachyon_core::safety::extract_filename_from_url;
use tachyon_sniffer::capture::identify_resource;
use tachyon_sniffer::{SnifferResource, redact_sensitive_params};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::commands::{AppError, resource_type_to_string};

/// 嗅探器应用服务
///
/// 负责嗅探器相关的业务逻辑：
/// - 资源管理：添加资源（去重、限数量、脱敏）
/// - 过滤器管理：添加过滤规则（去重、限长、限数量）
/// - 查询：获取资源列表
///
/// 由 Tauri command 层调用，command 层只负责参数解析和错误序列化。
pub struct SnifferService {
    /// 已捕获的资源列表
    resources: Arc<Mutex<Vec<SnifferResource>>>,
    /// 过滤规则列表
    filters: Arc<Mutex<Vec<String>>>,
}

impl SnifferService {
    /// 创建新的 SnifferService
    pub fn new() -> Self {
        Self {
            resources: Arc::new(Mutex::new(Vec::new())),
            filters: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// 获取所有资源（按发现时间倒序）
    pub async fn get_resources(&self) -> Vec<SnifferResource> {
        let store = self.resources.lock().await;
        store.iter().rev().cloned().collect()
    }

    /// 添加嗅探器资源
    ///
    /// 业务规则：
    /// - URL 必须匹配至少一条过滤规则（若过滤器非空）
    /// - 去重：已存在的 URL 不重复添加
    /// - 限数量：超过 MAX_SNIFFER_RESOURCES 时移除最早的资源
    /// - URL 脱敏后存储
    pub async fn add_resource(&self, url: String) {
        let filters = self.filters.lock().await;
        if !filters.is_empty() && !filters.iter().any(|f| url.contains(f.as_str())) {
            return;
        }
        drop(filters);

        let resource_type = identify_resource(&url);
        let file_name = extract_filename_from_url(&url);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let redacted_url = redact_sensitive_params(&url);
        let resource = SnifferResource {
            id: Uuid::new_v4().to_string(),
            url: redacted_url.clone(),
            download_url: url.clone(),
            file_name,
            resource_type: resource_type_to_string(resource_type).to_string(),
            file_size: None,
            content_type: None,
            discovered_at: now,
            source_page: None,
        };

        let mut store = self.resources.lock().await;

        if store.iter().any(|r| r.url == redacted_url) {
            return;
        }

        const MAX_SNIFFER_RESOURCES: usize = 1000;
        if store.len() >= MAX_SNIFFER_RESOURCES {
            store.remove(0);
        }

        tracing::info!(url = %tachyon_core::redact_url_for_log(&url), resource_type = %resource.resource_type, "捕获新资源");
        store.push(resource);
    }

    /// 添加过滤规则
    ///
    /// 业务规则：
    /// - 规则不能为空
    /// - 规则长度不能超过 MAX_FILTER_LENGTH
    /// - 规则数量不能超过 MAX_FILTER_COUNT
    /// - 规则不能重复
    pub async fn add_filter(&self, filter: String) -> Result<(), AppError> {
        if filter.is_empty() {
            return Err(AppError::Config("过滤规则不能为空".to_string()));
        }
        const MAX_FILTER_LENGTH: usize = 256;
        if filter.len() > MAX_FILTER_LENGTH {
            return Err(AppError::Config(format!(
                "过滤规则长度不能超过 {MAX_FILTER_LENGTH} 字符"
            )));
        }
        let mut filters = self.filters.lock().await;
        const MAX_FILTER_COUNT: usize = 100;
        if filters.len() >= MAX_FILTER_COUNT {
            return Err(AppError::Config(format!(
                "过滤规则数量已达上限 {MAX_FILTER_COUNT}"
            )));
        }
        if filters.contains(&filter) {
            return Err(AppError::Config("过滤规则已存在".to_string()));
        }
        tracing::info!(filter = %filter, "添加嗅探过滤规则");
        filters.push(filter);
        Ok(())
    }

    /// 清空所有资源
    pub async fn clear_resources(&self) {
        let mut store = self.resources.lock().await;
        store.clear();
    }
}

impl Default for SnifferService {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_get_resources_empty() {
        let service = SnifferService::new();
        let resources = service.get_resources().await;
        assert!(resources.is_empty());
    }

    #[tokio::test]
    async fn test_add_filter_duplicate_rejected() {
        let service = SnifferService::new();
        service
            .add_filter("cdn.example.com".to_string())
            .await
            .unwrap();
        let result = service.add_filter("cdn.example.com".to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("已存在"));
    }

    #[tokio::test]
    async fn test_add_filter_empty_string_fails() {
        let service = SnifferService::new();
        let result = service.add_filter(String::new()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("不能为空"));
    }

    #[tokio::test]
    async fn test_add_filter_too_long_fails() {
        let service = SnifferService::new();
        let long_filter = "a".repeat(257);
        let result = service.add_filter(long_filter).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("长度不能超过"));
    }

    #[tokio::test]
    async fn test_add_filter_max_count() {
        let service = SnifferService::new();
        for i in 0..100 {
            service.add_filter(format!("filter-{i}")).await.unwrap();
        }
        let result = service.add_filter("overflow".to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("上限"));
    }

    #[tokio::test]
    async fn test_add_resource() {
        let service = SnifferService::new();
        service
            .add_resource("http://example.com/video.mp4".to_string())
            .await;
        let resources = service.get_resources().await;
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].url, "http://example.com/video.mp4");
        assert_eq!(resources[0].resource_type, "video");
        assert_eq!(resources[0].file_name, "video.mp4");
    }

    #[tokio::test]
    async fn test_add_resource_duplicate_ignored() {
        let service = SnifferService::new();
        service
            .add_resource("http://example.com/file.zip".to_string())
            .await;
        service
            .add_resource("http://example.com/file.zip".to_string())
            .await;
        let resources = service.get_resources().await;
        assert_eq!(resources.len(), 1, "重复 URL 应被忽略");
    }

    #[tokio::test]
    async fn test_add_resource_with_filter() {
        let service = SnifferService::new();
        service
            .add_filter("cdn.example.com".to_string())
            .await
            .unwrap();
        service
            .add_resource("http://other.com/video.mp4".to_string())
            .await;
        assert_eq!(service.get_resources().await.len(), 0);
        service
            .add_resource("http://cdn.example.com/video.mp4".to_string())
            .await;
        assert_eq!(service.get_resources().await.len(), 1);
    }

    #[tokio::test]
    async fn test_clear_resources() {
        let service = SnifferService::new();
        service
            .add_resource("http://example.com/file.zip".to_string())
            .await;
        assert_eq!(service.get_resources().await.len(), 1);
        service.clear_resources().await;
        assert!(service.get_resources().await.is_empty());
    }
}
