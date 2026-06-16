use tachyon_sniffer::SnifferResource;

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

#[tauri::command]
pub async fn add_sniffer_filter(
    state: tauri::State<'_, AppState>,
    filter: String,
) -> Result<(), AppError> {
    state.service.sniffer_service.add_filter(filter).await
}

pub async fn add_sniffer_resource(state: &AppState, url: String) {
    state.service.sniffer_service.add_resource(url).await
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
        add_sniffer_resource(&state, "http://example.com/video.mp4".to_string()).await;
        let resources = state.service.sniffer_service.get_resources().await;
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].url, "http://example.com/video.mp4");
        assert_eq!(resources[0].resource_type, "video");
        assert_eq!(resources[0].file_name, "video.mp4");
    }

    #[tokio::test]
    async fn test_add_sniffer_resource_duplicate_ignored() {
        let state = test_state();
        add_sniffer_resource(&state, "http://example.com/file.zip".to_string()).await;
        add_sniffer_resource(&state, "http://example.com/file.zip".to_string()).await;
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
        add_sniffer_resource(&state, "http://other.com/video.mp4".to_string()).await;
        assert_eq!(state.service.sniffer_service.get_resources().await.len(), 0);
        add_sniffer_resource(&state, "http://cdn.example.com/video.mp4".to_string()).await;
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
}
