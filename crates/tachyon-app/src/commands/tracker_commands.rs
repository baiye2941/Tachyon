use std::time::{SystemTime, UNIX_EPOCH};

use tachyon_engine::tracker_source::{
    SeedTrackersResult, fetch_default_trackers as fetch_trackers, seed_trackers,
};
use tachyon_protocol::http::HttpClient;

use super::{AppError, AppState};

/// 按当前筛选条件从 trackers.run 拉取 tracker URL 列表。
#[tauri::command]
pub async fn fetch_default_trackers(
    _state: tauri::State<'_, AppState>,
    filters: tachyon_core::config::TrackerSourceFilters,
) -> Result<Vec<String>, AppError> {
    let client = HttpClient::with_timeouts(10, 30)?;
    let trackers = fetch_trackers(&client, &filters).await?;
    Ok(trackers)
}

/// 根据配置状态决定是否从 trackers.run 刷新 tracker 列表。
///
/// - tracker 列表为空时立即拉取；
/// - 自动管理开启且 TTL 到期时后台拉取；
/// - 其他情况直接返回当前状态。
#[tauri::command]
pub async fn seed_default_trackers(
    state: tauri::State<'_, AppState>,
) -> Result<SeedTrackersResult, AppError> {
    let client = HttpClient::with_timeouts(10, 30)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut config = state.domain.config.lock().await;
    let (updated, count) = seed_trackers(&mut config.magnet, &client, now).await?;

    if updated {
        let config_to_save = config.clone();
        drop(config);
        tokio::task::spawn_blocking(move || {
            crate::commands::config_commands::persist_config(&config_to_save)
        })
        .await
        .map_err(|e| AppError::Config(format!("持久化配置任务失败: {e}")))??;
    }

    Ok(SeedTrackersResult { updated, count })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_trackers_result_serializes_camel_case() {
        let result = SeedTrackersResult {
            updated: true,
            count: 42,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"updated\":true"));
        assert!(json.contains("\"count\":42"));
    }
}
