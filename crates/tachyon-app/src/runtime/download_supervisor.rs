//! 下载任务运行时管理器
//!
//! 负责下载任务的 spawn、JoinHandle 管理、控制命令通道和运行时资源清理。
//! 不包含业务逻辑（业务规则在 TaskService 中）。

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tokio::sync::watch;

use tachyon_engine::ConnectionPool;

use crate::AppState;
use crate::commands::TaskCommand;
use crate::commands::task_commands::task_fn;

/// 下载任务运行时管理器
///
/// 管理：
/// - `handles`: 每个下载任务的 Tokio JoinHandle
/// - `controls`: 每个下载任务的控制命令通道（TaskCommand）
/// - `connection_pool`: 全局连接池热替换句柄
///
/// 持有 `Arc<RwLock<Arc<ConnectionPool>>>` 而非直接的 `Arc<ConnectionPool>`:
/// `start_download` 在创建新任务时读锁内 clone 出当前 `Arc<ConnectionPool>`
/// 传给 task_fn。`update_config` 热重建时在写锁内替换内层 Arc,
/// 已启动任务持有的旧 pool 不受影响,新任务拿到新 pool。
pub struct DownloadSupervisor {
    /// 下载任务 JoinHandle
    pub(crate) handles: Arc<DashMap<String, tokio::task::JoinHandle<()>>>,
    /// 控制命令通道（TaskCommand，而非 DownloadState）
    pub(crate) command_channels: Arc<DashMap<String, watch::Sender<TaskCommand>>>,
    /// 审计 H-02:每 task 控制命令串行锁,防止 pause/resume 交错写 TaskInfo 与 watch
    pub(crate) command_locks: Arc<DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// 全局连接池热替换句柄
    pub(crate) connection_pool: Arc<tokio::sync::RwLock<Arc<ConnectionPool>>>,
}

impl DownloadSupervisor {
    /// 创建新的 DownloadSupervisor
    pub fn new(connection_pool: Arc<tokio::sync::RwLock<Arc<ConnectionPool>>>) -> Self {
        Self {
            handles: Arc::new(DashMap::new()),
            command_channels: Arc::new(DashMap::new()),
            command_locks: Arc::new(DashMap::new()),
            connection_pool,
        }
    }

    /// 获取/创建任务控制串行锁(审计 H-02)
    pub fn task_command_lock(&self, task_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.command_locks
            .entry(task_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
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

        let pool_handle = self.connection_pool.clone();
        // 切片2:从 AppState.infra 取全局 BufferPool 经 task_fn 注入到 DownloadTask,
        // 使 worker 用池化 buffer 写入磁盘(反压 + 内存有界)。
        // 审计 A-14:读锁 clone 任务启动时刻的池快照,热重建不影响运行中任务。
        let buffer_pool_handle = state.infra.buffer_pool.clone();
        let tid = task_id.to_string();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();

        let handle = tokio::spawn(async move {
            let _ = start_rx.await;
            // 读锁内 clone 出当前 Arc<ConnectionPool>:
            // 取的是任务启动时刻的 pool 快照,后续热重建不影响本任务。
            let pool_clone = pool_handle.read().await.clone();
            let buffer_pool_clone = buffer_pool_handle.read().await.clone();
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
    /// 返回 true 表示命令成功入队；false 表示任务不存在或
    /// receiver 已关闭（审计 H-03：不得忽略 `send` Err 伪成功，否则 Resume 会被永久吞掉）。
    pub fn send_command(&self, task_id: &str, command: TaskCommand) -> bool {
        if let Some(control) = self.command_channels.get(task_id) {
            control.send(command).is_ok()
        } else {
            false
        }
    }

    /// 检测任务是否有运行中的 task_fn(存在 control channel)
    ///
    /// 用于 `resume_task_inner` 区分两种恢复场景:
    /// - 有 channel:task_fn 仍在运行(被 Paused),直接 `send_command(Resume)` 即可。
    /// - 无 channel:task_fn 已退出(应用启动恢复的任务),需重新 `start_download` 激活,
    ///   否则 `send_command` 静默返回 false,Resume 信号丢失。
    pub fn has_running_task(&self, task_id: &str) -> bool {
        self.command_channels.contains_key(task_id)
    }

    /// 清理运行时资源
    ///
    /// 移除 JoinHandle 和控制命令通道。
    /// 注意：不清理 TaskInfo（由 TaskService 管理）。
    pub fn cleanup(&self, task_id: &str) {
        self.command_channels.remove(task_id);
        self.handles.remove(task_id);
        self.command_locks.remove(task_id);
    }

    /// 等待任务 JoinHandle 完成（带超时）
    ///
    /// 审计 H-04:删除/停止路径在删文件或 drop 运行时前应 await generation。
    /// - 从 maps 取出 handle,避免与并发 cleanup 竞态
    /// - 超时后 abort,并再短等以尽量 drain abort
    /// - 同时移除 command channel,防止旧 Cancel 通道泄漏
    pub async fn wait_for_handle(
        &self,
        task_id: &str,
        timeout: Duration,
    ) -> Option<Result<(), tokio::task::JoinError>> {
        let handle = self.handles.remove(task_id).map(|(_, h)| h);
        let Some(mut handle) = handle else {
            self.command_channels.remove(task_id);
            return None;
        };
        match tokio::time::timeout(timeout, &mut handle).await {
            Ok(result) => {
                self.command_channels.remove(task_id);
                Some(result)
            }
            Err(_) => {
                tracing::warn!(task_id = %task_id, ?timeout, "任务 JoinHandle 等待超时,abort");
                handle.abort();
                // abort 后尽量 drain,避免调用方立刻删文件时旧 task 仍持有写句柄
                let abort_grace =
                    Duration::from_secs(2).min(timeout.max(Duration::from_millis(100)));
                let _ = tokio::time::timeout(abort_grace, &mut handle).await;
                self.command_channels.remove(task_id);
                None
            }
        }
    }

    /// 删除路径默认等待活跃下载 quiesce 的上限
    pub const DELETE_QUIESCE_TIMEOUT: Duration = Duration::from_secs(15);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tachyon_engine::{ConnectionPool, PoolConfig};

    fn empty_supervisor() -> DownloadSupervisor {
        let pool = Arc::new(tokio::sync::RwLock::new(Arc::new(ConnectionPool::new(
            PoolConfig::default(),
        ))));
        DownloadSupervisor::new(pool)
    }

    /// 审计 H-03:receiver 已关闭时 send_command 必须返回 false(不得伪成功)
    #[test]
    fn test_send_command_returns_false_when_receiver_closed() {
        let supervisor = empty_supervisor();
        let (tx, rx) = watch::channel(TaskCommand::Start);
        supervisor.command_channels.insert("t-closed".into(), tx);
        drop(rx); // 关闭 receiver,模拟 session 已退出但 entry 未 cleanup

        let ok = supervisor.send_command("t-closed", TaskCommand::Resume);
        assert!(
            !ok,
            "H-03: receiver 关闭后 send_command 必须 false,否则 Resume 被永久吞掉"
        );
    }

    #[test]
    fn test_send_command_returns_true_when_receiver_alive() {
        let supervisor = empty_supervisor();
        let (tx, _rx) = watch::channel(TaskCommand::Start);
        supervisor.command_channels.insert("t-ok".into(), tx);
        assert!(supervisor.send_command("t-ok", TaskCommand::Pause));
    }

    #[test]
    fn test_send_command_returns_false_for_unknown_task() {
        let supervisor = empty_supervisor();
        assert!(!supervisor.send_command("missing", TaskCommand::Cancel));
    }
}
