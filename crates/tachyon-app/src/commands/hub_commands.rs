use super::AppError;

// ---------------------------------------------------------------------------
// Tauri commands (no inner functions -- logic is inline)
// ---------------------------------------------------------------------------

/// 列出 HuggingFace 仓库文件
#[tauri::command]
pub async fn list_repo_files(
    repo_id: String,
    revision: Option<String>,
) -> Result<Vec<tachyon_hub::api::HfFile>, AppError> {
    let rev = revision.unwrap_or_else(|| "main".to_string());
    let api = tachyon_hub::api::HubApi::from_env();
    api.list_files(&repo_id, &rev).await.map_err(AppError::Core)
}

/// 获取 HuggingFace 文件下载 URL
#[tauri::command]
pub async fn get_hf_download_url(
    repo_id: String,
    revision: Option<String>,
    file_path: String,
) -> Result<String, AppError> {
    let rev = revision.unwrap_or_else(|| "main".to_string());
    let api = tachyon_hub::api::HubApi::from_env();
    Ok(api.download_url(&repo_id, &rev, &file_path))
}
