//! 下载任务运行时管理器
//!
//! 负责下载任务的 spawn、JoinHandle 管理、控制命令通道和运行时资源清理。
//! 不包含业务逻辑（业务规则在 TaskService 中）。

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::watch;

use tachyon_engine::connection::ConnectionPool;

use crate::AppState;
use crate::commands::TaskCommand;
use crate::commands::task_commands::task_fn;

/// 下载任务运行时管理器
///
/// 管理：
/// - `handles`: 每个下载任务的 Tokio JoinHandle
/// - `controls`: 每个下载任务的控制命令通道（TaskCommand）
/// - `connection_pool`: 全局连接池
pub struct DownloadSupervisor {
    /// 下载任务 JoinHandle
    pub(crate) handles: Arc<DashMap<String, tokio::task::JoinHandle<()>>>,
    /// 控制命令通道（TaskCommand，而非 DownloadState）
    pub(crate) command_channels: Arc<DashMap<String, watch::Sender<TaskCommand>>>,
    /// 全局连接池
    pub(crate) connection_pool: Arc<ConnectionPool>,
}

impl DownloadSupervisor {
    /// 创建新的 DownloadSupervisor
    pub fn new(connection_pool: Arc<ConnectionPool>) -> Self {
        Self {
            handles: Arc::new(DashMap::new()),
            command_channels: Arc::new(DashMap::new()),
            connection_pool,
        }
    }

    /// 启动下载任务
    ///
    /// 创建 control channel 并 spawn task_fn，注册 JoinHandle。
    /// `start_tx/start_rx` 确保任务在所有资源注册完成后才开始执行。
    /// `preferred_file_name` 为用户在「新建任务」中显式传入的重命名(已 sanitize),
    /// 透传给引擎以在 probe 后覆盖协议侧文件名。
    #[allow(clippy::too_many_arguments)]
    pub fn start_download(
        &self,
        state: Arc<AppState>,
        task_id: &str,
        url: String,
        download_dir: String,
        download_config: tachyon_core::config::DownloadConfig,
        mirror_urls: Option<Vec<String>>,
        preferred_file_name: Option<String>,
    ) {
        let (control_tx, control_rx) = watch::channel(TaskCommand::Start);
        self.command_channels
            .insert(task_id.to_string(), control_tx);

        let pool_clone = self.connection_pool.clone();
        // 切片2:从 AppState.infra 取全局 BufferPool 经 task_fn 注入到 DownloadTask,
        // 使 worker 用池化 buffer 写入磁盘(反压 + 内存有界)。
        let buffer_pool_clone = state.infra.buffer_pool.clone();
        let tid = task_id.to_string();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();

        let handle = tokio::spawn(async move {
            let _ = start_rx.await;
            task_fn(
                state,
                tid,
                url,
                download_dir,
                download_config,
                pool_clone,
                buffer_pool_clone,
                control_rx,
                mirror_urls,
                preferred_file_name,
            )
            .await;
        });

        self.handles.insert(task_id.to_string(), handle);
        // start_tx.send 只在 start_rx 被 drop 时失败（即 spawned task 已被 abort），
        // 此时应清理已注册的资源
        if start_tx.send(()).is_err() {
            tracing::warn!(task_id = %task_id, "下载任务 spawn 后立即退出,清理资源");
            self.command_channels.remove(task_id);
            self.handles.remove(task_id);
            return;
        }

        tracing::info!(task_id = %task_id, "创建下载任务并启动后台下载");
    }

    /// 发送控制命令到指定任务
    ///
    /// 返回 true 表示命令成功发送，false 表示任务不存在或通道已关闭。
    pub fn send_command(&self, task_id: &str, command: TaskCommand) -> bool {
        if let Some(control) = self.command_channels.get(task_id) {
            let _ = control.send(command);
            true
        } else {
            false
        }
    }

    /// 清理运行时资源
    ///
    /// 移除 JoinHandle 和控制命令通道。
    /// 注意：不清理 TaskInfo（由 TaskService 管理）。
    pub fn cleanup(&self, task_id: &str) {
        self.command_channels.remove(task_id);
        self.handles.remove(task_id);
    }

    /// 等待任务 JoinHandle 完成（带超时）
    ///
    /// 用于在任务结束后等待 progress monitor 和 chunk reader 完成。
    pub async fn wait_for_handle(
        &self,
        task_id: &str,
        timeout: Duration,
    ) -> Option<Result<(), tokio::task::JoinError>> {
        let mut handle = self.handles.remove(task_id)?.1;
        match tokio::time::timeout(timeout, &mut handle).await {
            Ok(result) => Some(result),
            Err(_) => {
                tracing::warn!(task_id = %task_id, "任务 JoinHandle 等待超时");
                handle.abort();
                None
            }
        }
    }
}
