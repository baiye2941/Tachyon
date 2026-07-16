use tachyon_sniffer::SnifferResource;
use tachyon_sniffer::capture::CaptureConfig;

use super::{AppError, AppState};

// ---------------------------------------------------------------------------
// Tauri command wrappers
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_sniffer_resources(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<SnifferResource>, AppError> {
    Ok(state.service.sniffer_service.get_resources().await)
}

/// 审计 SEC-009:按嗅探资源 id 创建下载任务(原始 URL 不经 IPC 回前端)
#[tauri::command]
pub async fn create_task_from_sniffer(
    state: tauri::State<'_, AppState>,
    resource_id: String,
    download_dir: Option<String>,
    auto_start: Option<bool>,
) -> Result<String, AppError> {
    let resource = state
        .service
        .sniffer_service
        .get_resource_by_id(&resource_id)
        .await
        .ok_or_else(|| AppError::Config(format!("嗅探资源不存在: {resource_id}")))?;
    let url = resource.download_url;
    if url.is_empty() {
        return Err(AppError::Config("嗅探资源缺少下载 URL".into()));
    }
    crate::commands::task_commands::create_task_inner(
        &state,
        url,
        download_dir,
        None,
        None,
        auto_start.unwrap_or(true),
        None,
    )
    .await
}

#[tauri::command]
pub async fn add_sniffer_filter(
    state: tauri::State<'_, AppState>,
    filter: String,
) -> Result<(), AppError> {
    state.service.sniffer_service.add_filter(filter).await
}

#[tauri::command]
pub async fn add_sniffer_resource(
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
    url: String,
) -> Result<(), AppError> {
    if let Some(resource) = state.service.sniffer_service.add_resource(url).await {
        use tauri::Emitter;
        let _ = app_handle.emit("sniffer://resource-added", &resource);
    }
    Ok(())
}

/// 设置嗅探捕获配置(类型白名单、最小文件大小、URL 过滤器)
#[tauri::command]
pub async fn set_sniffer_capture_config(
    state: tauri::State<'_, AppState>,
    config: CaptureConfig,
) -> Result<(), AppError> {
    state
        .service
        .sniffer_service
        .set_capture_config(config)
        .await
}

/// 获取当前嗅探捕获配置
#[tauri::command]
pub async fn get_sniffer_capture_config(
    state: tauri::State<'_, AppState>,
) -> Result<CaptureConfig, AppError> {
    Ok(state.service.sniffer_service.capture_config().await)
}

/// 清空所有嗅探资源
#[tauri::command]
pub async fn clear_sniffer_resources(state: tauri::State<'_, AppState>) -> Result<(), AppError> {
    state.service.sniffer_service.clear_resources().await;
    Ok(())
}

/// 内部辅助:直接向嗅探器添加资源(供其他模块/测试调用,不经 Tauri 命令分发)
pub async fn add_sniffer_resource_inner(state: &AppState, url: String) {
    let _ = state.service.sniffer_service.add_resource(url).await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::resource_type_to_string;
    use super::super::tests::test_state;
    use super::*;
    use tachyon_sniffer::capture::ResourceType;

    #[tokio::test]
    async fn test_get_sniffer_resources_empty() {
        let state = test_state();
        let resources = state.service.sniffer_service.get_resources().await;
        assert!(resources.is_empty());
    }

    #[tokio::test]
    async fn test_create_task_from_sniffer_uses_backend_url() {
        let state = test_state();
        let resource = state
            .service
            .sniffer_service
            .add_resource("https://example.com/sniffer-sec009.bin?token=secret".into())
            .await
            .expect("应添加嗅探资源");
        let json = serde_json::to_string(&resource).unwrap();
        assert!(!json.contains("token=secret"), "IPC 序列化泄漏: {json}");
        assert!(
            !json.contains("downloadUrl"),
            "IPC 不得含 downloadUrl: {json}"
        );

        let task_id = crate::commands::task_commands::create_task_inner(
            &state,
            resource.download_url.clone(),
            None,
            None,
            None,
            false,
            None,
        )
        .await
        .expect("应按后端 download_url 建任务");
        let detail = crate::commands::task_commands::get_task_detail_inner(&state, task_id)
            .await
            .unwrap();
        assert!(
            detail.url.contains("sniffer-sec009.bin"),
            "任务 URL 应来自嗅探资源: {}",
            detail.url
        );
    }

    #[tokio::test]
    async fn test_add_sniffer_filter() {
        let state = test_state();
        state
            .service
            .sniffer_service
            .add_filter("cdn.example.com".to_string())
            .await
            .unwrap();
        let result = state
            .service
            .sniffer_service
            .add_filter("cdn.example.com".to_string())
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("已存在"));
    }

    #[tokio::test]
    async fn test_add_sniffer_filter_empty_string_fails() {
        let state = test_state();
        let result = state
            .service
            .sniffer_service
            .add_filter(String::new())
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("不能为空"));
    }

    #[tokio::test]
    async fn test_add_sniffer_filter_too_long_fails() {
        let state = test_state();
        let long_filter = "a".repeat(257);
        let result = state.service.sniffer_service.add_filter(long_filter).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("长度不能超过"));
    }

    #[tokio::test]
    async fn test_add_sniffer_filter_max_count() {
        let state = test_state();
        for i in 0..100 {
            state
                .service
                .sniffer_service
                .add_filter(format!("filter-{i}"))
                .await
                .unwrap();
        }
        let result = state
            .service
            .sniffer_service
            .add_filter("overflow".to_string())
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("上限"));
    }

    #[tokio::test]
    async fn test_add_sniffer_resource() {
        let state = test_state();
        add_sniffer_resource_inner(&state, "http://example.com/video.mp4".to_string()).await;
        let resources = state.service.sniffer_service.get_resources().await;
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].url, "http://example.com/video.mp4");
        assert_eq!(resources[0].resource_type, "video");
        assert_eq!(resources[0].file_name, "video.mp4");
    }

    #[tokio::test]
    async fn test_add_sniffer_resource_duplicate_ignored() {
        let state = test_state();
        add_sniffer_resource_inner(&state, "http://example.com/file.zip".to_string()).await;
        add_sniffer_resource_inner(&state, "http://example.com/file.zip".to_string()).await;
        let resources = state.service.sniffer_service.get_resources().await;
        assert_eq!(resources.len(), 1, "重复 URL 应被忽略");
    }

    #[tokio::test]
    async fn test_add_sniffer_resource_with_filter() {
        let state = test_state();
        state
            .service
            .sniffer_service
            .add_filter("cdn.example.com".to_string())
            .await
            .unwrap();
        add_sniffer_resource_inner(&state, "http://other.com/video.mp4".to_string()).await;
        assert_eq!(state.service.sniffer_service.get_resources().await.len(), 0);
        add_sniffer_resource_inner(&state, "http://cdn.example.com/video.mp4".to_string()).await;
        assert_eq!(state.service.sniffer_service.get_resources().await.len(), 1);
    }

    #[test]
    fn test_resource_type_to_string_all_variants() {
        assert_eq!(resource_type_to_string(ResourceType::Video), "video");
        assert_eq!(resource_type_to_string(ResourceType::Audio), "audio");
        assert_eq!(resource_type_to_string(ResourceType::Document), "document");
        assert_eq!(resource_type_to_string(ResourceType::Archive), "archive");
        assert_eq!(
            resource_type_to_string(ResourceType::Executable),
            "executable"
        );
        assert_eq!(resource_type_to_string(ResourceType::Image), "image");
        assert_eq!(resource_type_to_string(ResourceType::Model), "model");
        assert_eq!(resource_type_to_string(ResourceType::Other), "other");
    }

    #[tokio::test]
    async fn test_capture_config_round_trip() {
        let state = test_state();
        let mut cfg = state.service.sniffer_service.capture_config().await;
        // 禁用 Video,提高 min_size
        cfg.enabled_types.remove(&ResourceType::Video);
        cfg.min_size = 10_000;
        state
            .service
            .sniffer_service
            .set_capture_config(cfg.clone())
            .await
            .unwrap();
        let got = state.service.sniffer_service.capture_config().await;
        assert!(!got.enabled_types.contains(&ResourceType::Video));
        assert_eq!(got.min_size, 10_000);
    }

    #[tokio::test]
    async fn test_clear_sniffer_resources() {
        let state = test_state();
        add_sniffer_resource_inner(&state, "http://example.com/a.mp4".to_string()).await;
        add_sniffer_resource_inner(&state, "http://example.com/b.mp3".to_string()).await;
        assert_eq!(state.service.sniffer_service.get_resources().await.len(), 2);
        state.service.sniffer_service.clear_resources().await;
        assert!(
            state
                .service
                .sniffer_service
                .get_resources()
                .await
                .is_empty()
        );
    }
}
