//! 嗅探器应用服务
//!
//! 审计 A-07:资源与 CaptureConfig 的唯一 owner 是 `tachyon_sniffer::ResourceManager`。
//! 本服务仅作 Tauri/async 适配层,禁止再持有第二份资源/配置存储。

use std::sync::Arc;

use tachyon_sniffer::capture::CaptureConfig;
use tachyon_sniffer::{ResourceManager, SnifferResource};

use crate::commands::AppError;

/// 嗅探器应用服务(薄适配层)
pub struct SnifferService {
    manager: Arc<ResourceManager>,
}

impl SnifferService {
    /// 创建新的 SnifferService(内嵌默认 ResourceManager)
    pub fn new() -> Self {
        Self {
            manager: Arc::new(ResourceManager::default()),
        }
    }

    /// 注入已有 ResourceManager(测试/共享)
    pub fn from_manager(manager: Arc<ResourceManager>) -> Self {
        Self { manager }
    }

    /// 底层唯一 owner(供高级路径/测试)
    pub fn manager(&self) -> Arc<ResourceManager> {
        Arc::clone(&self.manager)
    }

    /// 获取当前捕获配置的克隆
    pub async fn capture_config(&self) -> CaptureConfig {
        self.manager.config()
    }

    /// 更新捕获配置(校验在 ResourceManager 内)
    pub async fn set_capture_config(&self, config: CaptureConfig) -> Result<(), AppError> {
        self.manager
            .set_config(config)
            .map_err(|e| AppError::Config(e.to_string()))
    }

    /// 按 id 取资源
    pub async fn get_resource_by_id(&self, id: &str) -> Option<SnifferResource> {
        self.manager.get_by_id(id)
    }

    /// 获取所有资源（按发现时间倒序）
    pub async fn get_resources(&self) -> Vec<SnifferResource> {
        self.manager.get_all()
    }

    /// 添加嗅探资源(手动 URL;无 size → 不应用 min_size)
    pub async fn add_resource(&self, url: String) -> Option<SnifferResource> {
        let res = self.manager.add_url(&url);
        if let Some(ref r) = res {
            tracing::info!(
                url = %tachyon_core::redact_url_for_log(&url),
                resource_type = %r.resource_type,
                "捕获新资源"
            );
        }
        res
    }

    /// 添加过滤规则
    pub async fn add_filter(&self, filter: String) -> Result<(), AppError> {
        self.manager
            .add_filter(filter)
            .map_err(|e| AppError::Config(e.to_string()))
    }

    /// 清空所有资源
    pub async fn clear_resources(&self) {
        self.manager.clear();
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

    #[tokio::test]
    async fn test_add_resource_returns_added_resource() {
        let service = SnifferService::new();
        let added = service
            .add_resource("http://example.com/movie.mp4".to_string())
            .await;
        assert!(added.is_some(), "新增资源应返回 Some");
        let res = added.unwrap();
        assert_eq!(res.file_name, "movie.mp4");
        assert_eq!(res.resource_type, "video");
    }

    #[tokio::test]
    async fn test_add_resource_returns_none_on_duplicate() {
        let service = SnifferService::new();
        let first = service
            .add_resource("http://example.com/file.zip".to_string())
            .await;
        let second = service
            .add_resource("http://example.com/file.zip".to_string())
            .await;
        assert!(first.is_some(), "首次添加应返回 Some");
        assert!(second.is_none(), "重复 URL 应返回 None");
    }

    #[tokio::test]
    async fn test_add_resource_returns_none_when_filtered_out() {
        let service = SnifferService::new();
        service
            .add_filter("cdn.example.com".to_string())
            .await
            .unwrap();
        let rejected = service
            .add_resource("http://other.com/video.mp4".to_string())
            .await;
        assert!(rejected.is_none(), "被过滤器拦截的 URL 应返回 None");
    }

    #[tokio::test]
    async fn test_add_resource_respects_disabled_type() {
        let service = SnifferService::new();
        let mut cfg = service.capture_config().await;
        cfg.enabled_types
            .remove(&tachyon_sniffer::capture::ResourceType::Video);
        service.set_capture_config(cfg).await.unwrap();
        let result = service
            .add_resource("http://example.com/movie.mp4".to_string())
            .await;
        assert!(result.is_none(), "禁用 Video 类型后不应捕获视频");
        assert!(service.get_resources().await.is_empty());
    }

    #[tokio::test]
    async fn test_add_resource_allows_enabled_type() {
        let service = SnifferService::new();
        let result = service
            .add_resource("http://example.com/movie.mp4".to_string())
            .await;
        assert!(result.is_some(), "默认配置应捕获视频");
    }

    /// A-07:adapter 与 ResourceManager 是同一 owner
    #[tokio::test]
    async fn test_a07_service_uses_single_resource_manager() {
        let service = SnifferService::new();
        service
            .add_resource("http://example.com/a.zip".to_string())
            .await;
        assert_eq!(service.manager().count(), 1);
        assert_eq!(service.get_resources().await.len(), 1);
        // 直接经 manager 添加也应可见
        service
            .manager()
            .add_url("http://example.com/b.mp4")
            .expect("manager 添加");
        assert_eq!(service.get_resources().await.len(), 2);
    }

    // P1-22-5: set_capture_config 必须与 add_filter 应用相同校验

    #[tokio::test]
    async fn test_set_capture_config_rejects_empty_filter() {
        let service = SnifferService::new();
        let mut cfg = service.capture_config().await;
        cfg.url_filters.push(String::new());
        let result = service.set_capture_config(cfg).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("不能为空"));
    }

    #[tokio::test]
    async fn test_set_capture_config_rejects_too_long_filter() {
        let service = SnifferService::new();
        let mut cfg = service.capture_config().await;
        cfg.url_filters.push("a".repeat(257));
        let result = service.set_capture_config(cfg).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("长度不能超过"));
    }

    #[tokio::test]
    async fn test_set_capture_config_rejects_too_many_filters() {
        let service = SnifferService::new();
        let mut cfg = service.capture_config().await;
        cfg.url_filters = (0..101).map(|i| format!("filter-{i}")).collect();
        let result = service.set_capture_config(cfg).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("上限"));
    }

    #[tokio::test]
    async fn test_set_capture_config_rejects_duplicate_filters() {
        let service = SnifferService::new();
        let mut cfg = service.capture_config().await;
        cfg.url_filters = vec!["same".into(), "same".into()];
        let result = service.set_capture_config(cfg).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("已存在"));
    }
}
