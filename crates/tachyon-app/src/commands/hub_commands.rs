use super::{
    AppError, AppState, FileVerifyResult, HfTaskMeta, LocalModel, LocalModelFile, ModelFavorite,
    VerifyStatus,
};
use std::path::Path;
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
/// 获取当前 HubConfig(source_mode + token)
///
/// 锁失败视为配置损坏,降级为默认 Mirror。
async fn current_hub_config(state: &AppState) -> tachyon_core::config::HubConfig {
    state.domain.config.lock().await.hub.clone()
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
    let hub = current_hub_config(&state).await;
    let api = tachyon_hub::api::HubApi::from_config(&hub)
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
    let hub = current_hub_config(&state).await;
    let api = tachyon_hub::api::HubApi::from_config(&hub)
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
    let hub = current_hub_config(&state).await;
    let api = tachyon_hub::api::HubApi::from_config(&hub)
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
    let hub = current_hub_config(&state).await;
    let api = tachyon_hub::api::HubApi::from_config(&hub)
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

/// 单个文件的校验任务快照。
///
/// 脱离 DashMap 借用后使用,自包含校验所需信息,便于并发执行。
enum VerifyJob {
    /// LFS 文件: 计算 sha256 并与 oid 比对(oid 已去除 "sha256:" 前缀)
    Lfs {
        file_path: String,
        save_path: String,
        oid: String,
    },
    /// 普通文件: 比对本地与远程文件大小
    Size {
        file_path: String,
        save_path: String,
        size: u64,
    },
    /// 远程文件信息缺失
    Missing { file_path: String },
}

/// 对单个文件执行校验,返回 (file_path, 校验状态)。
///
/// LFS 文件计算 sha256 与期望 oid 比对;普通文件比对大小。
/// 文件不存在直接判为失败,不进行后续读取。
/// metadata 为轻量 syscall,直接在 async 上下文调用,无需 spawn_blocking。
async fn compute_verify_status(job: VerifyJob) -> (String, VerifyStatus) {
    match job {
        VerifyJob::Missing { file_path } => (
            file_path,
            VerifyStatus::Failed("远程文件信息缺失".to_string()),
        ),
        VerifyJob::Lfs {
            file_path,
            save_path,
            oid,
        } => {
            if !Path::new(&save_path).exists() {
                return (file_path, VerifyStatus::Failed("文件不存在".to_string()));
            }
            let status =
                match tachyon_engine::sha256_file(std::path::Path::new(&save_path), 8192).await {
                    Ok(hash) => {
                        if hash.eq_ignore_ascii_case(&oid) {
                            VerifyStatus::Verified
                        } else {
                            VerifyStatus::Failed(format!("sha256 不匹配: 期望 {oid}, 实际 {hash}"))
                        }
                    }
                    Err(e) => VerifyStatus::Failed(format!("校验计算失败: {e}")),
                };
            (file_path, status)
        }
        VerifyJob::Size {
            file_path,
            save_path,
            size,
        } => {
            if !Path::new(&save_path).exists() {
                return (file_path, VerifyStatus::Failed("文件不存在".to_string()));
            }
            // 普通文件大小比对:metadata 为轻量 syscall,直接在 async 上下文调用
            let actual_size = std::fs::metadata(&save_path).map(|m| m.len()).unwrap_or(0);
            let status = if actual_size == size {
                VerifyStatus::Verified
            } else {
                VerifyStatus::Failed(format!("大小不匹配: 期望 {size}, 实际 {actual_size}"))
            };
            (file_path, status)
        }
    }
}

/// 校验模型文件
///
/// LFS 文件使用 sha256 校验，普通文件使用文件大小比对。
/// 各文件校验并发执行(上限 4),避免多个大文件串行 sha256 耗时过长。
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
    let hub = current_hub_config(&state).await;
    let api = tachyon_hub::api::HubApi::from_config(&hub)
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

    // 1. 遍历 task_repository 收集校验任务快照。
    //    DashMap 迭代器为同步借用,不能跨 await 持有,故先快照为 Vec,
    //    释放借用后再并发执行各文件的校验。
    let mut jobs: Vec<(VerifyJob, std::time::Instant)> = Vec::new();
    for entry in state.domain.task_repository.iter() {
        let task = entry.value();
        let Some(ref meta) = task.hf_meta else {
            continue;
        };
        if meta.repo_id != repo_id {
            continue;
        }

        let job = match remote_map.get(&meta.file_path) {
            Some(remote) => {
                if let Some(ref lfs) = remote.lfs {
                    VerifyJob::Lfs {
                        file_path: meta.file_path.clone(),
                        save_path: task.save_path.clone(),
                        oid: lfs.oid.trim_start_matches("sha256:").to_string(),
                    }
                } else {
                    VerifyJob::Size {
                        file_path: meta.file_path.clone(),
                        save_path: task.save_path.clone(),
                        size: remote.size,
                    }
                }
            }
            None => VerifyJob::Missing {
                file_path: meta.file_path.clone(),
            },
        };
        jobs.push((job, std::time::Instant::now()));
    }

    // 2. 并发执行校验。
    //    使用 JoinSet 驱动各 future,配合 Semaphore 限制并发度为 4,
    //    避免同时打开过多文件句柄。每个 future 独立计时(elapsed_ms)。
    let concurrency = 4usize;
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut join_set = tokio::task::JoinSet::new();

    for (job, start) in jobs {
        let permit = semaphore.clone();
        join_set.spawn(async move {
            // 先提取 file_path 用于结果构造(无论成功失败都需要),
            // 避免在 permit 获取失败的提前返回分支中无法访问 job 内字段
            let file_path = match &job {
                VerifyJob::Lfs { file_path, .. }
                | VerifyJob::Size { file_path, .. }
                | VerifyJob::Missing { file_path } => file_path.clone(),
            };
            // 申请并发许可,获取后执行校验;释放后自动让出给下一个任务
            let _permit = match permit.acquire_owned().await {
                Ok(p) => p,
                // 信号量关闭视为任务被取消,直接返回失败
                Err(_) => {
                    return (
                        file_path,
                        VerifyStatus::Failed("并发调度被取消".to_string()),
                        start,
                    );
                }
            };
            let (_path, status) = compute_verify_status(job).await;
            (file_path, status, start)
        });
    }

    // 3. 收集结果。JoinSet::join_next 完成顺序不确定,但每条结果携带
    //    file_path,无序收集不影响最终语义。
    while let Some(res) = join_set.join_next().await {
        match res {
            Ok((file_path, status, start)) => results.push(FileVerifyResult {
                path: file_path,
                status,
                elapsed_ms: start.elapsed().as_millis() as u64,
            }),
            // 任务 panic:记为失败
            Err(join_err) => results.push(FileVerifyResult {
                path: String::new(),
                status: VerifyStatus::Failed(format!("校验任务异常: {join_err}")),
                elapsed_ms: 0,
            }),
        }
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
    let hub = current_hub_config(&state).await;
    let cached_info = match tachyon_hub::api::HubApi::from_config(&hub) {
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
    force_mirror: Option<bool>,
) -> Result<Vec<String>, AppError> {
    validate_repo_id(&repo_id)?;
    let rev = revision.unwrap_or_else(|| "main".to_string());
    validate_revision(&rev)?;

    if file_paths.is_empty() {
        return Err(AppError::Config("文件列表不能为空".into()));
    }

    let hub = current_hub_config(&state).await;
    let api = tachyon_hub::api::HubApi::from_config(&hub)
        .map_err(|e| AppError::Config(format!("Hub API 初始化失败: {e}")))?;

    let force_mirror = force_mirror.unwrap_or(false);

    let mut task_ids = Vec::new();
    for file_path in file_paths {
        validate_file_path(&file_path)?;
        let mut url = api.download_url(&repo_id, &rev, &file_path);
        // 审计 FT-07:显式镜像时仍注入 HfTaskMeta;URL 走 hf-mirror resolve
        if force_mirror {
            url = super::rewrite_hf_url(&url, tachyon_core::config::HfSourceMode::Mirror);
        }
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
            true,
            Some(hf_meta),
        )
        .await?;
        task_ids.push(id);
    }

    Ok(task_ids)
}
