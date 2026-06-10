use super::AppError;

// ---------------------------------------------------------------------------
// 输入验证 (W-19)
// ---------------------------------------------------------------------------

/// 验证 HuggingFace repo_id 格式: `owner/repo`
///
/// 防止路径遍历 (`..`) 和注入攻击。
fn validate_repo_id(repo_id: &str) -> Result<(), AppError> {
    if repo_id.is_empty() || repo_id.len() > 256 {
        return Err(AppError::Config("repo_id 长度必须在 1~256 之间".into()));
    }
    if repo_id.contains("..") || repo_id.contains('\\') {
        return Err(AppError::Config("repo_id 包含非法字符".into()));
    }
    let parts: Vec<&str> = repo_id.split('/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(AppError::Config("repo_id 格式必须为 'owner/repo'".into()));
    }
    Ok(())
}

/// 验证 revision 参数: 仅允许字母、数字、`-`、`_`、`.`
fn validate_revision(rev: &str) -> Result<(), AppError> {
    if rev.is_empty() || rev.len() > 128 {
        return Err(AppError::Config("revision 长度必须在 1~128 之间".into()));
    }
    if !rev
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(AppError::Config("revision 包含非法字符".into()));
    }
    Ok(())
}

/// 验证文件路径: 不允许路径遍历和绝对路径
fn validate_file_path(path: &str) -> Result<(), AppError> {
    if path.is_empty() || path.len() > 1024 {
        return Err(AppError::Config("file_path 长度必须在 1~1024 之间".into()));
    }
    if path.contains("..") || path.starts_with('/') || path.starts_with('\\') {
        return Err(AppError::Config(
            "file_path 不允许路径遍历或绝对路径".into(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// 列出 HuggingFace 仓库文件
#[tauri::command]
pub async fn list_repo_files(
    repo_id: String,
    revision: Option<String>,
) -> Result<Vec<tachyon_hub::api::HfFile>, AppError> {
    validate_repo_id(&repo_id)?;
    let rev = revision.unwrap_or_else(|| "main".to_string());
    validate_revision(&rev)?;
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
    validate_repo_id(&repo_id)?;
    let rev = revision.unwrap_or_else(|| "main".to_string());
    validate_revision(&rev)?;
    validate_file_path(&file_path)?;
    let api = tachyon_hub::api::HubApi::from_env();
    Ok(api.download_url(&repo_id, &rev, &file_path))
}
