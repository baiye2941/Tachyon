use super::{
    AppError, AppState, FileVerifyResult, HfTaskMeta, LocalModel, LocalModelFile, ModelFavorite,
    VerifyStatus,
};
use std::path::Path;
use tachyon_core::config::HfSourceMode;
use tachyon_hub::classify_file;
use tauri::State;

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

/// 读取当前 HF 源模式(从 AppConfig,配置驱动)
///
/// 锁失败视为配置损坏,降级为默认 Mirror。
async fn current_hf_source_mode(state: &AppState) -> HfSourceMode {
    state.domain.config.lock().await.hub.source_mode
}

/// 列出 HuggingFace 仓库文件
#[tauri::command]
pub async fn list_repo_files(
    state: State<'_, AppState>,
    repo_id: String,
    revision: Option<String>,
) -> Result<Vec<tachyon_hub::api::HfFile>, AppError> {
    validate_repo_id(&repo_id)?;
    let rev = revision.unwrap_or_else(|| "main".to_string());
    validate_revision(&rev)?;
    let mode = current_hf_source_mode(&state).await;
    let api = tachyon_hub::api::HubApi::from_mode(mode)
        .map_err(|e| AppError::Config(format!("Hub API 初始化失败: {e}")))?;
    api.list_files(&repo_id, &rev).await.map_err(AppError::Core)
}

/// 获取 HuggingFace 文件下载 URL
#[tauri::command]
pub async fn get_hf_download_url(
    state: State<'_, AppState>,
    repo_id: String,
    revision: Option<String>,
    file_path: String,
) -> Result<String, AppError> {
    validate_repo_id(&repo_id)?;
    let rev = revision.unwrap_or_else(|| "main".to_string());
    validate_revision(&rev)?;
    validate_file_path(&file_path)?;
    let mode = current_hf_source_mode(&state).await;
    let api = tachyon_hub::api::HubApi::from_mode(mode)
        .map_err(|e| AppError::Config(format!("Hub API 初始化失败: {e}")))?;
    Ok(api.download_url(&repo_id, &rev, &file_path))
}

/// 获取模型元数据
#[tauri::command]
pub async fn get_model_info(
    state: State<'_, AppState>,
    repo_id: String,
    revision: Option<String>,
) -> Result<tachyon_hub::api::HfModelInfo, AppError> {
    validate_repo_id(&repo_id)?;
    let rev = revision.unwrap_or_else(|| "main".to_string());
    validate_revision(&rev)?;
    let mode = current_hf_source_mode(&state).await;
    let api = tachyon_hub::api::HubApi::from_mode(mode)
        .map_err(|e| AppError::Config(format!("Hub API 初始化失败: {e}")))?;
    api.model_info(&repo_id, &rev).await.map_err(AppError::Core)
}

/// 搜索模型
#[tauri::command]
pub async fn search_models(
    state: State<'_, AppState>,
    query: String,
    limit: Option<u32>,
) -> Result<Vec<tachyon_hub::api::HfModelInfo>, AppError> {
    if query.is_empty() {
        return Err(AppError::Config("搜索查询不能为空".into()));
    }
    let limit = limit.unwrap_or(20).min(50);
    let mode = current_hf_source_mode(&state).await;
    let api = tachyon_hub::api::HubApi::from_mode(mode)
        .map_err(|e| AppError::Config(format!("Hub API 初始化失败: {e}")))?;
    api.search_models(&query, limit)
        .await
        .map_err(AppError::Core)
}

/// 扫描本地已下载模型
///
/// 从 TaskRepository 筛选有 HfTaskMeta 的 Completed 任务，按 repo_id 聚合。
#[tauri::command]
pub async fn scan_local_models(state: State<'_, AppState>) -> Result<Vec<LocalModel>, AppError> {
    let mut models: std::collections::HashMap<String, LocalModel> =
        std::collections::HashMap::new();

    for entry in state.domain.task_repository.iter() {
        let task = entry.value();
        if task.status != tachyon_core::types::DownloadState::Completed {
            continue;
        }
        let Some(ref meta) = task.hf_meta else {
            continue;
        };

        let model = models
            .entry(meta.repo_id.clone())
            .or_insert_with(|| LocalModel {
                repo_id: meta.repo_id.clone(),
                revision: meta.revision.clone(),
                local_path: Path::new(&task.save_path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default(),
                files: Vec::new(),
                total_size: 0,
                downloaded_at: Some(task.created_at.clone()),
                metadata: None,
            });

        let file_exists = Path::new(&task.save_path).exists();
        let category = classify_file(&meta.file_path);
        model.files.push(LocalModelFile {
            path: meta.file_path.clone(),
            local_path: task.save_path.clone(),
            size: task.file_size.unwrap_or(0),
            category,
            lfs_oid: meta.lfs_oid.clone(),
            verify_status: if file_exists {
                VerifyStatus::Unverified
            } else {
                VerifyStatus::Failed("文件不存在".to_string())
            },
            exists: file_exists,
        });
        model.total_size += task.file_size.unwrap_or(0);
    }

    Ok(models.into_values().collect())
}

/// 校验模型文件
///
/// LFS 文件使用 sha256 校验，普通文件使用文件大小比对。
#[tauri::command]
pub async fn verify_model(
    state: State<'_, AppState>,
    repo_id: String,
    revision: Option<String>,
) -> Result<Vec<FileVerifyResult>, AppError> {
    validate_repo_id(&repo_id)?;
    let rev = revision.unwrap_or_else(|| "main".to_string());
    validate_revision(&rev)?;

    // 获取远程文件列表用于比对
    let mode = current_hf_source_mode(&state).await;
    let api = tachyon_hub::api::HubApi::from_mode(mode)
        .map_err(|e| AppError::Config(format!("Hub API 初始化失败: {e}")))?;
    let remote_files = api
        .list_files(&repo_id, &rev)
        .await
        .map_err(AppError::Core)?;
    let remote_map: std::collections::HashMap<String, tachyon_hub::api::HfFile> = remote_files
        .into_iter()
        .map(|f| (f.path.clone(), f))
        .collect();

    let mut results = Vec::new();

    for entry in state.domain.task_repository.iter() {
        let task = entry.value();
        let Some(ref meta) = task.hf_meta else {
            continue;
        };
        if meta.repo_id != repo_id {
            continue;
        }

        let path = &task.save_path;
        let start = std::time::Instant::now();
        let status = if !Path::new(path).exists() {
            VerifyStatus::Failed("文件不存在".to_string())
        } else if let Some(remote) = remote_map.get(&meta.file_path) {
            if let Some(ref lfs) = remote.lfs {
                // LFS 文件: sha256 校验
                let verifier = tachyon_crypto::cpu::CpuVerifier::sha256();
                match verifier
                    .compute_hash_from_path(std::path::Path::new(path), 8192)
                    .await
                {
                    Ok(hash) => {
                        let expected = lfs.oid.trim_start_matches("sha256:");
                        if hash.eq_ignore_ascii_case(expected) {
                            VerifyStatus::Verified
                        } else {
                            VerifyStatus::Failed(format!(
                                "sha256 不匹配: 期望 {expected}, 实际 {hash}"
                            ))
                        }
                    }
                    Err(e) => VerifyStatus::Failed(format!("校验计算失败: {e}")),
                }
            } else {
                // 普通文件: 大小比对
                let actual_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                if actual_size == remote.size {
                    VerifyStatus::Verified
                } else {
                    VerifyStatus::Failed(format!(
                        "大小不匹配: 期望 {}, 实际 {actual_size}",
                        remote.size
                    ))
                }
            }
        } else {
            VerifyStatus::Failed("远程文件信息缺失".to_string())
        };

        results.push(FileVerifyResult {
            path: meta.file_path.clone(),
            status,
            elapsed_ms: start.elapsed().as_millis() as u64,
        });
    }

    Ok(results)
}

/// 列出收藏模型
#[tauri::command]
pub async fn list_model_favorites(
    state: State<'_, AppState>,
) -> Result<Vec<ModelFavorite>, AppError> {
    // favorites_store 底层为 FileStore,使用 std::fs 同步 I/O(含 fsync),
    // 直接在 async 上下文调用会阻塞 tokio worker 线程,故包裹 spawn_blocking。
    let store = state.infra.favorites_store.clone();
    tokio::task::spawn_blocking(move || -> Result<Vec<ModelFavorite>, AppError> {
        let keys = store
            .list_by_prefix("fav_")
            .map_err(|e| AppError::Config(format!("读取收藏列表失败: {e}")))?;

        let mut favorites = Vec::new();
        for key in keys {
            let repo_id = key.strip_prefix("fav_").unwrap_or(&key).to_string();
            let cached: Option<tachyon_hub::api::HfModelInfo> = store
                .get(&key)
                .map_err(|e| AppError::Config(format!("读取收藏数据失败: {e}")))?;

            let added_at = store
                .get_raw(&key)
                .ok()
                .flatten()
                .and_then(|json| {
                    serde_json::from_str::<serde_json::Value>(&json)
                        .ok()
                        .and_then(|v| {
                            v.get("addedAt")
                                .and_then(|a| a.as_str().map(|s| s.to_string()))
                        })
                })
                .unwrap_or_else(|| chrono::Local::now().to_rfc3339());

            favorites.push(ModelFavorite {
                repo_id,
                added_at,
                cached_info: cached,
            });
        }

        Ok(favorites)
    })
    .await
    .map_err(|e| AppError::Config(format!("收藏列表读取任务失败: {e}")))?
}

/// 添加收藏模型
#[tauri::command]
pub async fn add_model_favorite(
    state: State<'_, AppState>,
    repo_id: String,
) -> Result<(), AppError> {
    validate_repo_id(&repo_id)?;
    let key = format!("fav_{repo_id}");

    // 尝试缓存模型元数据
    let mode = current_hf_source_mode(&state).await;
    let cached_info = match tachyon_hub::api::HubApi::from_mode(mode) {
        Ok(api) => api.model_info(&repo_id, "main").await.ok(),
        Err(_) => None,
    };

    let favorite = ModelFavorite {
        repo_id: repo_id.clone(),
        added_at: chrono::Local::now().to_rfc3339(),
        cached_info,
    };

    // favorites_store 底层为 FileStore 同步 I/O(含 fsync),包裹 spawn_blocking 避免阻塞 tokio。
    let store = state.infra.favorites_store.clone();
    tokio::task::spawn_blocking(move || {
        store
            .put(&key, &favorite)
            .map_err(|e| AppError::Config(format!("保存收藏失败: {e}")))
    })
    .await
    .map_err(|e| AppError::Config(format!("保存收藏任务失败: {e}")))??;

    Ok(())
}

/// 移除收藏模型
#[tauri::command]
pub async fn remove_model_favorite(
    state: State<'_, AppState>,
    repo_id: String,
) -> Result<(), AppError> {
    validate_repo_id(&repo_id)?;
    let key = format!("fav_{repo_id}");
    // favorites_store 底层为 FileStore 同步 I/O,包裹 spawn_blocking 避免阻塞 tokio。
    let store = state.infra.favorites_store.clone();
    tokio::task::spawn_blocking(move || {
        store
            .delete(&key)
            .map_err(|e| AppError::Config(format!("删除收藏失败: {e}")))
    })
    .await
    .map_err(|e| AppError::Config(format!("删除收藏任务失败: {e}")))??;
    Ok(())
}

/// 批量创建 HF 下载任务
///
/// 为每个文件创建下载任务并注入 HfTaskMeta。
#[tauri::command]
pub async fn batch_create_hf_tasks(
    state: State<'_, AppState>,
    repo_id: String,
    revision: Option<String>,
    file_paths: Vec<String>,
    download_dir: Option<String>,
) -> Result<Vec<String>, AppError> {
    validate_repo_id(&repo_id)?;
    let rev = revision.unwrap_or_else(|| "main".to_string());
    validate_revision(&rev)?;

    if file_paths.is_empty() {
        return Err(AppError::Config("文件列表不能为空".into()));
    }

    let mode = current_hf_source_mode(&state).await;
    let api = tachyon_hub::api::HubApi::from_mode(mode)
        .map_err(|e| AppError::Config(format!("Hub API 初始化失败: {e}")))?;

    let mut task_ids = Vec::new();
    for file_path in file_paths {
        validate_file_path(&file_path)?;
        let url = api.download_url(&repo_id, &rev, &file_path);
        let hf_meta = HfTaskMeta {
            repo_id: repo_id.clone(),
            revision: rev.clone(),
            file_path: file_path.clone(),
            lfs_oid: None,
        };
        let id = super::task_commands::create_task_inner(
            &state,
            url,
            download_dir.clone(),
            None,
            None,
            Some(hf_meta),
        )
        .await?;
        task_ids.push(id);
    }

    Ok(task_ids)
}
