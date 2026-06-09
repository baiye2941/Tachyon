use tachyon_core::filename::extract_filename_from_url;
use tachyon_sniffer::SnifferResource;
use tachyon_sniffer::capture::identify_resource;
use uuid::Uuid;

use super::{AppError, AppState, resource_type_to_string};

// ---------------------------------------------------------------------------
// Tauri command wrappers
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_sniffer_resources(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<SnifferResource>, AppError> {
    get_sniffer_resources_inner(&state).await
}

#[tauri::command]
pub async fn add_sniffer_filter(
    state: tauri::State<'_, AppState>,
    filter: String,
) -> Result<(), AppError> {
    add_sniffer_filter_inner(&state, filter).await
}

pub async fn add_sniffer_resource(state: &AppState, url: String) {
    let filters = state.sniffer_filters.lock().await;
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
    let resource = SnifferResource {
        id: Uuid::new_v4().to_string(),
        url: url.clone(),
        file_name,
        resource_type: resource_type_to_string(resource_type).to_string(),
        file_size: None,
        content_type: None,
        discovered_at: now,
        source_page: None,
    };

    let mut store = state.sniffer.lock().await;

    if store.iter().any(|r| r.url == url) {
        return;
    }

    const MAX_SNIFFER_RESOURCES: usize = 1000;
    if store.len() >= MAX_SNIFFER_RESOURCES {
        store.remove(0);
    }

    tracing::info!(url = %tachyon_core::redact_url_for_log(&url), resource_type = %resource.resource_type, "捕获新资源");
    store.push(resource);
}

// ---------------------------------------------------------------------------
// Inner implementations
// ---------------------------------------------------------------------------

async fn get_sniffer_resources_inner(
    state: &AppState,
) -> Result<Vec<SnifferResource>, AppError> {
    let store = state.sniffer.lock().await;
    Ok(store.iter().rev().cloned().collect())
}

async fn add_sniffer_filter_inner(state: &AppState, filter: String) -> Result<(), AppError> {
    if filter.is_empty() {
        return Err(AppError::Config("过滤规则不能为空".to_string()));
    }
    let mut filters = state.sniffer_filters.lock().await;
    if filters.contains(&filter) {
        return Err(AppError::Config("过滤规则已存在".to_string()));
    }
    tracing::info!(filter = %filter, "添加嗅探过滤规则");
    filters.push(filter);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::test_state;
    use super::super::resource_type_to_string;
    use tachyon_sniffer::capture::ResourceType;

    #[tokio::test]
    async fn test_get_sniffer_resources_empty() {
        let state = test_state();
        let resources = get_sniffer_resources_inner(&state).await.unwrap();
        assert!(resources.is_empty());
    }

    #[tokio::test]
    async fn test_add_sniffer_filter() {
        let state = test_state();
        add_sniffer_filter_inner(&state, "cdn.example.com".to_string())
            .await
            .unwrap();
        let result = add_sniffer_filter_inner(&state, "cdn.example.com".to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("已存在"));
    }

    #[tokio::test]
    async fn test_add_sniffer_filter_empty_string_fails() {
        let state = test_state();
        let result = add_sniffer_filter_inner(&state, String::new()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("不能为空"));
    }

    #[tokio::test]
    async fn test_add_sniffer_resource() {
        let state = test_state();
        add_sniffer_resource(&state, "http://example.com/video.mp4".to_string()).await;
        let resources = get_sniffer_resources_inner(&state).await.unwrap();
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
        let resources = get_sniffer_resources_inner(&state).await.unwrap();
        assert_eq!(resources.len(), 1, "重复 URL 应被忽略");
    }

    #[tokio::test]
    async fn test_add_sniffer_resource_with_filter() {
        let state = test_state();
        add_sniffer_filter_inner(&state, "cdn.example.com".to_string())
            .await
            .unwrap();
        add_sniffer_resource(&state, "http://other.com/video.mp4".to_string()).await;
        assert_eq!(get_sniffer_resources_inner(&state).await.unwrap().len(), 0);
        add_sniffer_resource(&state, "http://cdn.example.com/video.mp4".to_string()).await;
        assert_eq!(get_sniffer_resources_inner(&state).await.unwrap().len(), 1);
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
        assert_eq!(resource_type_to_string(ResourceType::Other), "other");
    }
}
