//! 任务应用服务
//!
//! 封装任务相关的业务规则，从 Tauri command 层提取的纯逻辑层。
//! 不直接依赖 Tauri 框架，可被 CLI/daemon/headless API 复用。
//!
//! TaskService 与 AppState 共享同一个 [`TaskRepository`]，
//! 确保所有读取/写入都操作同一份内存数据。

use std::sync::Arc;

use tachyon_core::config::{AppConfig, DownloadConfig};
use tachyon_core::safety::{extract_filename_from_url, redact_url_for_log, sanitize_filename};
use tachyon_core::types::DownloadState;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::commands::config_commands::authorize_download_dir;
use crate::commands::{
    AppError, TaskInfo, build_download_config, now_iso8601, validate_download_url,
};
use crate::repository::TaskRepository;
use crate::task_store::{TaskStore, task_info_to_snapshot};

/// 任务创建结果
///
/// 包含创建任务后所需的全部信息，供 DownloadSupervisor 启动下载。
pub struct TaskCreation {
    pub task_id: String,
    pub url: String,
    pub download_dir: String,
    pub download_config: DownloadConfig,
    pub mirror_urls: Option<Vec<String>>,
}

/// 任务应用服务
///
/// 负责任务相关的业务逻辑：
/// - 创建任务：URL 校验、镜像校验、并发门控、去重检查、目录授权、TaskInfo 创建、snapshot 持久化
/// - 状态变更：暂停/恢复/取消/删除的前置条件校验与 TaskInfo 更新
/// - 查询：任务列表与详情
///
/// 由 Tauri command 层调用，command 层只负责参数解析和错误序列化。
pub struct TaskService {
    /// 内存中的任务表（与 AppState 共享同一 TaskRepository 实例）
    pub(crate) task_repository: TaskRepository,
    /// 应用配置
    pub(crate) config: Arc<Mutex<AppConfig>>,
    /// 任务持久化存储
    pub(crate) task_store: Arc<TaskStore>,
    /// 任务创建锁：保证去重检查 + 并发计数 + 插入的原子性
    pub(crate) create_task_lock: Arc<Mutex<()>>,
    /// 缓存的下载目录，避免在热路径上获取 config 锁
    ///
    /// persist_snapshot 是高频调用(每次任务状态变更触发),而 download_dir
    /// 仅在 config 变更时更新(极低频)。RwLock 允许多个读者并发读取,
    /// 比 Mutex 更适合读多写少场景。
    cached_download_dir: Arc<RwLock<String>>,
}

impl TaskService {
    /// 创建新的 TaskService
    pub fn new(
        task_repository: TaskRepository,
        config: Arc<Mutex<AppConfig>>,
        task_store: Arc<TaskStore>,
        create_task_lock: Arc<Mutex<()>>,
    ) -> Self {
        // 同步获取初始 download_dir:try_new() 中 config 刚创建,
        // 此时 Mutex 无竞争,lock() 不会阻塞。这是一个初始化时的一次性操作。
        let initial_download_dir = config
            .try_lock()
            .map(|cfg| cfg.download.download_dir.clone())
            .unwrap_or_default();
        Self {
            task_repository,
            config,
            task_store,
            create_task_lock,
            cached_download_dir: Arc::new(RwLock::new(initial_download_dir)),
        }
    }

    /// 更新缓存的下载目录(config 变更时调用)
    ///
    /// 当用户通过 update_config 修改 download_dir 后,需调用此方法
    /// 同步缓存,避免 persist_snapshot 读取到过期的目录路径。
    pub async fn update_cached_download_dir(&self, new_dir: String) {
        *self.cached_download_dir.write().await = new_dir;
    }

    /// 创建下载任务
    ///
    /// 执行完整的业务规则校验后创建 TaskInfo 并持久化初始 snapshot。
    /// 返回 `TaskCreation` 供 DownloadSupervisor 启动下载。
    pub async fn create_task(
        &self,
        url: &str,
        download_dir: Option<&str>,
        mirror_urls: Option<&[String]>,
        file_name: Option<&str>,
    ) -> Result<TaskCreation, AppError> {
        validate_download_url(url)?;

        // 对每个镜像 URL 执行与主 URL 相同的 SSRF 防护验证
        if let Some(mirrors) = mirror_urls {
            for mirror in mirrors {
                validate_download_url(mirror)
                    .map_err(|e| AppError::Config(format!("镜像 URL 验证失败: {e}")))?;
            }
        }

        // 提前获取配置和下载目录,避免在检查-插入间隙中 await(消除 TOCTOU 竞态)
        // 同时预校验 max_concurrent_fragments,避免锁外失败需要回滚 tasks.insert
        let (max_tasks, download_dir_str, download_config) = {
            let mut cfg = self.config.lock().await;
            if cfg.download.max_concurrent_fragments == 0 {
                return Err(AppError::Config(
                    "max_concurrent_fragments 不能为 0".to_string(),
                ));
            }
            let max_tasks = cfg.max_concurrent_tasks as usize;
            let requested = download_dir
                .map(|s| s.to_string())
                .unwrap_or_else(|| cfg.download.download_dir.clone());
            let authorized = match authorize_download_dir(&cfg, &requested) {
                Ok(dir) => dir,
                Err(_) if download_dir.is_some() => {
                    // 用户通过对话框明确选择了目录,但不在 authorized_dirs 中
                    // 执行基本安全验证后自动授权该目录
                    let validated =
                        crate::commands::config_commands::validate_explicit_download_dir(
                            &requested,
                        )?;
                    cfg.download.authorized_dirs.push(validated.clone());
                    // 将新的授权目录持久化,避免重启后丢失
                    let config_to_save = cfg.clone();
                    if let Err(e) =
                        crate::commands::config_commands::persist_config(&config_to_save)
                    {
                        tracing::warn!(error = %e, "自动授权目录后持久化配置失败");
                    } else {
                        tracing::info!(dir = %validated, "已自动授权下载目录并持久化配置");
                    }
                    // 重新授权(此时目录已在 authorized_dirs 中)
                    authorize_download_dir(&cfg, &requested)?
                }
                Err(e) => return Err(e),
            };
            let config = build_download_config(&cfg, &authorized);
            (max_tasks, authorized, config)
        };

        let task_id = Uuid::new_v4().to_string();
        let file_name = match file_name {
            Some(n) if !n.trim().is_empty() => sanitize_filename(n),
            _ => extract_filename_from_url(url),
        };
        let created_at = now_iso8601();
        let redacted_url = redact_url_for_log(url);

        let task = TaskInfo {
            id: task_id.clone(),
            url: redacted_url.clone(),
            file_name,
            file_size: None,
            downloaded: 0,
            speed: 0,
            status: DownloadState::Pending,
            progress: 0.0,
            fragments_total: 0,
            fragments_done: 0,
            created_at,
            save_path: download_dir_str.clone(),
            error_reason: None,
            retry_count: 0,
        };

        // 使用互斥锁保证 check-and-insert 的原子性
        {
            let _create_guard = self.create_task_lock.lock().await;

            // 单次遍历同时完成去重检查和活跃任务计数,避免两次 O(n) 全表扫描
            let mut url_exists = false;
            let mut active_count: usize = 0;
            for r in self.task_repository.iter() {
                let t = r.value();
                if !url_exists
                    && t.url == redacted_url
                    && t.status != DownloadState::Cancelled
                    && t.status != DownloadState::Completed
                    && t.status != DownloadState::Failed
                {
                    url_exists = true;
                }
                if t.status == DownloadState::Downloading || t.status == DownloadState::Pending {
                    active_count += 1;
                }
            }

            if url_exists {
                return Err(AppError::TaskAlreadyExists(
                    "相同 URL 的下载任务已存在".to_string(),
                ));
            }
            if active_count >= max_tasks {
                return Err(AppError::Config(format!(
                    "已达最大并发任务数({max_tasks}),请等待现有任务完成"
                )));
            }
            // 在锁保护下立即插入,消除竞态窗口
            self.task_repository.insert(task_id.clone(), task);
        }

        // 持久化初始 snapshot
        if let Some(task) = self
            .task_repository
            .get(&task_id)
            .map(|r| r.value().clone())
        {
            let save_path = std::path::Path::new(&download_dir_str)
                .join(&task.file_name)
                .to_string_lossy()
                .to_string();
            let snapshot = task_info_to_snapshot(
                &task,
                save_path,
                0,
                vec![],
                std::collections::HashMap::new(),
                None,
                None,
            );
            if let Err(e) = self.task_store.save_snapshot(&snapshot) {
                tracing::warn!(task_id = %task_id, error = %e, "保存初始快照失败");
            }
        }

        Ok(TaskCreation {
            task_id,
            url: url.to_string(),
            download_dir: download_dir_str.to_string(),
            download_config,
            mirror_urls: mirror_urls.map(|v| v.to_vec()),
        })
    }

    /// 暂停任务
    pub async fn pause_task(&self, task_id: &str) -> Result<(), AppError> {
        let mut task = self
            .task_repository
            .get_mut(task_id)
            .ok_or_else(|| AppError::TaskNotFound(task_id.to_string()))?;
        match task.status {
            DownloadState::Pending | DownloadState::Downloading => {
                task.status = DownloadState::Paused;
                task.speed = 0;
                tracing::info!(task_id = %task_id, "暂停任务");
            }
            other => return Err(AppError::Config(format!("当前状态 '{}' 不允许暂停", other))),
        }
        drop(task);

        // 持久化暂停状态
        self.persist_snapshot(task_id, None).await;
        Ok(())
    }

    /// 恢复任务
    pub async fn resume_task(&self, task_id: &str) -> Result<(), AppError> {
        {
            let mut task = self
                .task_repository
                .get_mut(task_id)
                .ok_or_else(|| AppError::TaskNotFound(task_id.to_string()))?;
            if task.status == DownloadState::Paused {
                task.status = DownloadState::Downloading;
                tracing::info!(task_id = %task_id, "恢复任务");
            } else {
                return Err(AppError::Config(format!(
                    "仅暂停状态可恢复,当前状态: '{}'",
                    task.status
                )));
            }
        }

        self.persist_snapshot(task_id, None).await;
        Ok(())
    }

    /// 取消任务
    pub async fn cancel_task(&self, task_id: &str) -> Result<(), AppError> {
        {
            let mut task = self
                .task_repository
                .get_mut(task_id)
                .ok_or_else(|| AppError::TaskNotFound(task_id.to_string()))?;
            match task.status {
                DownloadState::Completed | DownloadState::Cancelled => {
                    return Err(AppError::Config(format!("任务已{},无法取消", task.status)));
                }
                _ => {
                    task.status = DownloadState::Cancelled;
                    task.speed = 0;
                    tracing::info!(task_id = %task_id, "取消任务");
                }
            }
        }

        self.persist_snapshot(task_id, None).await;
        Ok(())
    }

    /// 删除任务（仅终态可删除）
    ///
    /// 删除任务记录的同时,清理持久化快照和已下载的本地文件。
    /// 文件/快照清理失败仅记录警告,不阻断任务记录删除。
    pub fn delete_task(&self, task_id: &str) -> Result<(), AppError> {
        let task = self
            .task_repository
            .get(task_id)
            .ok_or_else(|| AppError::TaskNotFound(task_id.to_string()))?;
        match task.status {
            DownloadState::Completed | DownloadState::Cancelled | DownloadState::Failed => {
                let save_path = task.save_path.clone();
                drop(task);
                self.task_repository.remove(task_id);

                // 清理断点续传快照
                match self.task_store.remove_snapshot(task_id) {
                    Ok(true) => {
                        tracing::debug!(task_id = %task_id, "已删除任务快照");
                    }
                    Ok(false) => {
                        tracing::debug!(task_id = %task_id, "任务快照不存在,跳过清理");
                    }
                    Err(e) => {
                        tracing::warn!(task_id = %task_id, error = %e, "删除任务快照失败");
                    }
                }

                // 清理已下载文件(文件可能已被用户手动删除,失败仅警告)
                if !save_path.is_empty() {
                    let path = std::path::Path::new(&save_path);
                    if path.exists() {
                        if let Err(e) = std::fs::remove_file(path) {
                            tracing::warn!(
                                task_id = %task_id,
                                path = %save_path,
                                error = %e,
                                "删除任务文件失败"
                            );
                        } else {
                            tracing::info!(task_id = %task_id, path = %save_path, "已删除任务文件");
                        }
                    } else {
                        tracing::debug!(task_id = %task_id, path = %save_path, "任务文件不存在,跳过清理");
                    }
                }

                tracing::info!(task_id = %task_id, "删除任务");
                Ok(())
            }
            other => Err(AppError::Config(format!(
                "当前状态 '{}' 不允许删除,请先取消任务",
                other
            ))),
        }
    }

    /// 获取任务列表
    pub fn get_task_list(&self) -> Vec<TaskInfo> {
        self.task_repository
            .iter()
            .map(|r| r.value().clone())
            .collect()
    }

    /// 获取任务详情
    pub fn get_task_detail(&self, task_id: &str) -> Result<TaskInfo, AppError> {
        self.task_repository
            .get(task_id)
            .map(|r| r.value().clone())
            .ok_or(AppError::TaskNotFound(task_id.to_string()))
    }

    /// 更新任务状态
    pub fn update_task_status(&self, task_id: &str, new_status: DownloadState) {
        if let Some(mut task) = self.task_repository.get_mut(task_id) {
            task.status = new_status;
            if matches!(
                new_status,
                DownloadState::Completed | DownloadState::Failed | DownloadState::Cancelled
            ) {
                task.speed = 0;
            }
        }
    }

    /// 持久化任务快照
    async fn persist_snapshot(&self, task_id: &str, fail_reason: Option<String>) {
        // 1. 同步更新内存中 TaskInfo 的 error_reason,前端查询时立即可见
        if let Some(mut task) = self.task_repository.get_mut(task_id) {
            task.error_reason = fail_reason.clone();
        }

        let task = { self.task_repository.get(task_id).map(|r| r.value().clone()) };
        if let Some(task) = task {
            let existing = self.task_store.load_snapshot(task_id).ok().flatten();
            let save_path = if let Some(snapshot) = existing.as_ref() {
                snapshot.save_path.clone()
            } else {
                // 热路径:使用缓存的 download_dir,避免获取 config 锁
                let download_dir = self.cached_download_dir.read().await.clone();
                std::path::Path::new(&download_dir)
                    .join(&task.file_name)
                    .to_string_lossy()
                    .to_string()
            };
            let mut snapshot = task_info_to_snapshot(
                &task,
                save_path,
                0,
                vec![],
                std::collections::HashMap::new(),
                None,
                None,
            );
            if let Some(existing) = existing {
                snapshot.fragment_size = existing.fragment_size;
                snapshot.completed_fragments = existing.completed_fragments;
                snapshot.partial_fragments = existing.partial_fragments;
                snapshot.etag = existing.etag;
                snapshot.last_modified = existing.last_modified;
                snapshot.retry_count = existing.retry_count;
            }
            snapshot.fail_reason = fail_reason;
            if let Err(e) = self.task_store.save_snapshot(&snapshot) {
                tracing::warn!(task_id = %task_id, error = %e, "保存任务状态快照失败");
            }
        }
    }
}
