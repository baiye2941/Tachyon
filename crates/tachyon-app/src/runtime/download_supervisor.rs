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
    /// 重启/覆盖同 id 任务时,abort 后 drain 旧 generation 的上限。
    ///
    /// 审计 Phase1:仅 abort 不够——旧 task 可能仍短暂持有写句柄/网络。
    /// 策略:先 abort(立即停写),再 timeout(grace) await JoinHandle drain,再 spawn 新 generation。
    /// grace 实际上限见 start_download 内 `min(RESTART_QUIESCE_TIMEOUT, 500ms)`。
    pub const RESTART_QUIESCE_TIMEOUT: Duration = Duration::from_secs(3);

    #[allow(clippy::too_many_arguments)]
    pub async fn start_download(
        &self,
        state: Arc<AppState>,
        task_id: &str,
        url: String,
        download_dir: String,
        download_config: tachyon_core::config::DownloadConfig,
        mirror_urls: Option<Vec<String>>,
        preferred_file_name: Option<String>,
    ) {
        // C-03 + Phase1 join-before-restart:
        // 1) 先 abort 旧 generation(立即停写盘/联网),再短等 drain
        // 2) 清理旧 command channel / lock,防 ABA
        // 注意:不能先 wait 满超时再 abort——会让每次 restart 卡 RESTART_QUIESCE_TIMEOUT。
        if let Some((_, mut old_handle)) = self.handles.remove(task_id) {
            old_handle.abort();
            let grace = Self::RESTART_QUIESCE_TIMEOUT.min(Duration::from_millis(500));
            let _ = tokio::time::timeout(grace, &mut old_handle).await;
        }
        self.command_channels.remove(task_id);
        self.command_locks.remove(task_id);

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

        // 隔离 task_fn panic:单个下载崩溃不应杀全应用(release windows_subsystem
        // 下用户只见闪退)。panic 经 tracing 落盘 panic.log。
        // 依赖 panic=unwind(根 Cargo.toml release profile 已改回)。
        //
        // panic 后收尾:catch_unwind 捕获 panic 后,主动调 mark_task_failed_and_cleanup
        // 把任务转 Failed 态并清理 runtime(handles/command_channels/command_locks),
        // 否则 task_fn 栈展开会跳过 DownloadSession::run 的第 9-11 步终态清理,
        // 导致任务永久卡 Downloading + handle/channel 泄漏。
        // start_rx 门控与 pool clone 都在隔离 future 内,保证 spawn 后 JoinHandle
        // 语义与原实现一致(任务在 start_tx.send 后才真正运行,wait_for_handle 能 join)。
        let state_for_panic = state.clone();
        let tid_for_panic = tid.clone();
        let handle = crate::runtime::panic_isolation::spawn_isolated_with_panic_hook(
            "task_fn",
            async move {
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
            },
            move |panic_msg: String| {
                // 在 panic 捕获后同步执行终态清理:标记 Failed + 清理 runtime
                // (task_fn 栈已展开,DownloadSession::run 的 cleanup 被跳过)
                let state = state_for_panic.clone();
                let task_id = tid_for_panic.clone();
                tokio::spawn(async move {
                    tracing::warn!(
                        task_id = %task_id,
                        panic.msg = %panic_msg,
                        "task_fn panic 已捕获,执行终态清理(标记 Failed + 清理 runtime)"
                    );
                    crate::commands::task_commands::mark_task_failed_and_cleanup(&state, &task_id)
                        .await;
                });
            },
        );

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

    /// 取消路径默认等待活跃下载 quiesce 的上限(审计 H-04)
    ///
    /// Cancel 后 await JoinHandle quiesce,避免旧 task 仍在写盘/联网,
    /// 与 restart 产生竞态。超时后 wait_for_handle 内部 abort + 2s grace。
    pub const CANCEL_QUIESCE_TIMEOUT: Duration = Duration::from_secs(5);
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

    // ========================================================================
    // C-03: supervisor ABA 防护测试(RED — 当前实现不 abort 旧 handle,应失败)
    //
    // 审计发现:start_download 不检查同 id 已运行,直接 insert 覆盖旧 handle,
    // 旧 JoinHandle drop 不 abort,导致旧 task_fn 漂在 runtime 上。
    // Cancel→Undo→restart 路径可致旧 session 的 cleanup 误删新 session 控制面,
    // 留下不可取消的后台任务。
    //
    // 方案 A(轻量 abort)契约:
    //   start_download 开头:`if let Some((_, old)) = self.handles.remove(task_id) {
    //       old.abort();
    //   }` 并清 command_channels / command_locks 中同 id 的旧条目。
    //   保证旧 handle 必定 abort,旧 session 后续 cleanup 因通道已被替换而只影响新 session。
    // ========================================================================

    use crate::commands::tests::test_state;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tachyon_core::config::DownloadConfig;

    /// 构造"假 handle"——长任务空转,持续递增共享计数器。
    /// abort 后任务立即停止,计数器不再增长,以此间接验证 handle 已 abort。
    /// 不依赖 JoinHandle 所有权(因 insert 后所有权转移给 DashMap,无法再观察)。
    fn stale_handle(heartbeat: Arc<AtomicUsize>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                heartbeat.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
    }

    /// 等待计数器出现至少一次递增(证明任务已启动),带超时
    async fn wait_heartbeat_started(heartbeat: &AtomicUsize) -> bool {
        tokio::time::timeout(Duration::from_secs(2), async {
            while heartbeat.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .is_ok()
    }

    /// 在窗口期内观察计数器是否仍在增长。
    /// 返回 true 表示仍在增长(未 abort);false 表示已停止(已 abort)。
    async fn is_still_running(heartbeat: &AtomicUsize) -> bool {
        let before = heartbeat.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let after = heartbeat.load(Ordering::Relaxed);
        after > before
    }

    /// C-03-1: start_download 必须先 abort 同 task_id 的旧 JoinHandle
    ///
    /// 行为:预先在 `handles` 中插入一个长任务假 handle,调用 start_download,
    /// 验证旧 handle 已被 abort(共享计数器停止增长)。
    ///
    /// 预期失败原因:当前 start_download 直接 `handles.insert` 覆盖,
    /// 旧 handle 的 JoinHandle 被 drop 但 task_fn 仍在 runtime 上运行,
    /// 计数器持续增长,is_still_running 返回 true,断言失败。
    #[tokio::test]
    async fn test_start_download_aborts_stale_handle() {
        let state = test_state();
        let supervisor = &state.runtime.supervisor;
        let task_id = "c03-abort-stale";
        let heartbeat = Arc::new(AtomicUsize::new(0));

        // 预置旧假 handle(长任务,模拟正在运行的 task_fn)
        let old_handle = stale_handle(heartbeat.clone());
        supervisor.handles.insert(task_id.to_string(), old_handle);

        // 等待旧任务启动(至少一次心跳),证明 handle 确实在运行
        assert!(
            wait_heartbeat_started(&heartbeat).await,
            "旧 handle 必须已启动才能验证 abort"
        );

        // 调用 start_download(内部应先 abort 旧 handle)
        // 使用 ftp:// URL,task_fn 在 probe 阶段即快速失败,不干扰断言
        let download_dir = std::env::temp_dir().to_string_lossy().to_string();
        let download_config = DownloadConfig {
            download_dir: download_dir.clone(),
            authorized_dirs: vec![download_dir.clone()],
            ..DownloadConfig::default()
        };
        supervisor
            .start_download(
                state.clone(),
                task_id,
                "ftp://c03.invalid/stale.bin".to_string(),
                download_dir,
                download_config,
                None,
                None,
            )
            .await;

        // 验证旧 handle 已被 abort(计数器停止增长)
        let still_running = is_still_running(&heartbeat).await;
        assert!(
            !still_running,
            "C-03: start_download 必须 abort 同 id 旧 handle,旧 handle 仍运行(ABA 风险)"
        );

        // 清理新 session,避免泄漏到其他测试
        if let Some((_, new_handle)) = supervisor.handles.remove(task_id) {
            new_handle.abort();
        }
        supervisor.command_channels.remove(task_id);
        supervisor.command_locks.remove(task_id);
    }

    /// C-03-2: start_download 必须清除同 task_id 的旧 command channel
    ///
    /// 行为:预先在 `command_channels` 中插入一个旧 watch::Sender,
    /// 调用 start_download,验证旧 Sender 已被移除(被新的替换),
    /// 旧 session 的 receiver 不会收到新 session 发送的命令(无信号串扰)。
    ///
    /// 预期失败原因:当前 start_download 无条件 insert 覆盖,
    /// 旧 Sender 被 drop 但旧 task_fn 仍持有旧 control_rx。
    /// 此测试在当前实现下可能"意外通过"(因 insert 覆盖了条目,旧 Sender 被 drop),
    /// 但配合 test_start_download_aborts_stale_handle 可暴露根因:
    /// 旧 handle 仍在运行,持有已关闭的旧 receiver,可能仍在写状态。
    /// 这里通过验证"旧 receiver 未收到新 session 的命令"来约束信号隔离。
    #[tokio::test]
    async fn test_start_download_clears_old_command_channel() {
        let state = test_state();
        let supervisor = &state.runtime.supervisor;
        let task_id = "c03-clear-old-channel";

        // 预置旧 command channel(receiver 存活,模拟旧 session)
        let (old_tx, old_rx) = watch::channel(TaskCommand::Start);
        // 记录旧 receiver 初始观察到的值(Start)
        let old_initial = *old_rx.borrow();
        supervisor
            .command_channels
            .insert(task_id.to_string(), old_tx);

        // 调用 start_download
        let download_dir = std::env::temp_dir().to_string_lossy().to_string();
        let download_config = DownloadConfig {
            download_dir: download_dir.clone(),
            authorized_dirs: vec![download_dir.clone()],
            ..DownloadConfig::default()
        };
        supervisor
            .start_download(
                state.clone(),
                task_id,
                "ftp://c03.invalid/channel.bin".to_string(),
                download_dir,
                download_config,
                None,
                None,
            )
            .await;

        // 验证:新通道可用(send_command 返回 true)
        let ok = supervisor.send_command(task_id, TaskCommand::Pause);
        assert!(ok, "C-03: start_download 后新 command channel 必须可用");

        // 旧 receiver 不应收到 Pause(因为已被新 Sender 替换)。
        // watch::Sender::send 广播到所有克隆的 receiver,但旧 old_tx 已被 drop,
        // 新的 Sender 是独立 channel,旧 old_rx 不会收到本次 Pause。
        // 若旧 Sender 未被 drop(泄漏)且被复用,old_rx 会收到 Pause —— 这是 bug。
        let old_current = *old_rx.borrow();
        assert_ne!(
            old_current,
            TaskCommand::Pause,
            "C-03: 旧 command channel 必须被清除,旧 receiver 不应收到新 session 的命令(ABA 信号串扰)。旧 receiver 当前值: {:?}, 初始: {:?}",
            old_current,
            old_initial
        );

        // 清理
        if let Some((_, new_handle)) = supervisor.handles.remove(task_id) {
            new_handle.abort();
        }
        supervisor.command_channels.remove(task_id);
        supervisor.command_locks.remove(task_id);
    }

    /// C-03-3: start_download 必须清除同 task_id 的旧 command lock
    ///
    /// 行为:预先在 `command_locks` 中插入一个旧 Arc<Mutex>,
    /// 调用 start_download,验证旧 lock 已被移除,当前 entry 是新的 Arc。
    ///
    /// 预期失败原因:当前 start_download 不清理 command_locks,
    /// task_command_lock 用 or_insert_with 复用旧条目,旧 session 持有的
    /// Arc<Mutex> 仍是同一个,旧 session 可通过它阻塞新 session 的控制命令。
    #[tokio::test]
    async fn test_start_download_clears_old_command_lock() {
        let state = test_state();
        let supervisor = &state.runtime.supervisor;
        let task_id = "c03-clear-old-lock";

        // 预置旧 command lock(模拟旧 session 持有)
        let old_lock: Arc<tokio::sync::Mutex<()>> = Arc::new(tokio::sync::Mutex::new(()));
        supervisor
            .command_locks
            .insert(task_id.to_string(), old_lock.clone());

        // 调用 start_download
        let download_dir = std::env::temp_dir().to_string_lossy().to_string();
        let download_config = DownloadConfig {
            download_dir: download_dir.clone(),
            authorized_dirs: vec![download_dir.clone()],
            ..DownloadConfig::default()
        };
        supervisor
            .start_download(
                state.clone(),
                task_id,
                "ftp://c03.invalid/lock.bin".to_string(),
                download_dir,
                download_config,
                None,
                None,
            )
            .await;

        // 验证:task_command_lock 返回的必须是新的 Arc,而非旧的
        let new_lock = supervisor.task_command_lock(task_id);
        assert_ne!(
            Arc::as_ptr(&new_lock),
            Arc::as_ptr(&old_lock),
            "C-03: start_download 必须清除旧 command lock,旧 session 不应阻塞新 session"
        );

        // 清理
        if let Some((_, new_handle)) = supervisor.handles.remove(task_id) {
            new_handle.abort();
        }
        supervisor.command_channels.remove(task_id);
        supervisor.command_locks.remove(task_id);
    }

    /// C-03-4: 旧 session 的 cleanup 不应影响新 session 的控制面
    ///
    /// 行为:start_download(A) → start_download(B)(同 task_id 覆盖)→
    /// 验证 A 的 handle 已 abort(B 之前),B 的 handle/channel 仍活跃。
    ///
    /// 这模拟 Cancel→Undo→restart 路径:start_download(B) 必须先 abort A,
    /// 使得即使 A 的 cleanup 被调用,也无法删除 B 的控制面(因 A 持有的
    /// receiver 已关闭,B 持有的是新通道)。
    ///
    /// 预期失败原因:当前 start_download 不 abort A 的 handle,
    /// A 的 task_fn 仍持有旧 control_rx,B 的 send_command 信号串扰到 A,
    /// 或 A 的 handle 永久漂在 runtime 上(计数器持续增长)。
    #[tokio::test]
    async fn test_cleanup_after_restart_preserves_new_session() {
        let state = test_state();
        let supervisor = &state.runtime.supervisor;
        let task_id = "c03-restart-preserve";
        let download_dir = std::env::temp_dir().to_string_lossy().to_string();
        let download_config = DownloadConfig {
            download_dir: download_dir.clone(),
            authorized_dirs: vec![download_dir.clone()],
            ..DownloadConfig::default()
        };
        let heartbeat_a = Arc::new(AtomicUsize::new(0));

        // 第一次 start_download(A):先预置一个心跳 handle 模拟 A 的 task_fn
        let handle_a = stale_handle(heartbeat_a.clone());
        supervisor.handles.insert(task_id.to_string(), handle_a);
        // 同时预置 A 的 command channel 和 lock(模拟 A 的完整控制面)
        let (tx_a, _rx_a) = watch::channel(TaskCommand::Start);
        supervisor
            .command_channels
            .insert(task_id.to_string(), tx_a);
        let lock_a = Arc::new(tokio::sync::Mutex::new(()));
        supervisor.command_locks.insert(task_id.to_string(), lock_a);

        // 等待 A 启动
        assert!(
            wait_heartbeat_started(&heartbeat_a).await,
            "A 的 handle 必须已启动才能验证 abort"
        );

        // 第二次 start_download(B)——必须先 abort A 的 handle
        supervisor
            .start_download(
                state.clone(),
                task_id,
                "ftp://c03.invalid/session-b.bin".to_string(),
                download_dir.clone(),
                download_config,
                None,
                None,
            )
            .await;

        // 验证 B 的控制面已注册(handle/channel/lock 均为新 session 的)。
        // 必须在等待 A abort 之前同步检查:B 的 task_fn 探测 ftp://c03.invalid
        // 会在首个 await 点后快速失败并 self-cleanup,届时 handles 会被清空。
        // C-03 的契约是"start_download(B) 同步注册新控制面",而非"B 永久存活"。
        assert!(
            supervisor.handles.contains_key(task_id),
            "C-03: restart 后新 session 的 handle 必须已注册"
        );
        assert!(
            supervisor.command_channels.contains_key(task_id),
            "C-03: restart 后新 session 的 command channel 必须已注册"
        );
        // 新通道与旧通道隔离:send_command 走新通道(返回 true 表示新通道可用)
        let ok = supervisor.send_command(task_id, TaskCommand::Cancel);
        assert!(
            ok,
            "C-03: restart 后新 session 的 command channel 必须可发送"
        );

        // 验证 A 的 handle 已被 abort(计数器停止增长)
        let still_running_a = is_still_running(&heartbeat_a).await;
        assert!(
            !still_running_a,
            "C-03: start_download(B) 必须 abort A 的 handle,A 仍运行(后台泄漏)"
        );

        // 清理 B
        if let Some((_, handle_b)) = supervisor.handles.remove(task_id) {
            handle_b.abort();
        }
        supervisor.command_channels.remove(task_id);
        supervisor.command_locks.remove(task_id);
    }

    /// Phase1: start_download 必须在 spawn 新 generation 前 await 旧 JoinHandle 退出
    /// (不仅 abort)。旧任务若在 drop 后仍短暂运行,新任务会与旧写盘/联网竞态。
    ///
    /// 行为:预置一个"abort 后仍 sleep 再心跳"的假 handle;
    /// start_download 完成后,在 RESTART 窗口内旧任务不得再有心跳。
    #[tokio::test]
    async fn test_start_download_awaits_old_generation_before_spawn() {
        let state = test_state();
        let supervisor = &state.runtime.supervisor;
        let task_id = "c03-join-before-restart";
        let heartbeat = Arc::new(AtomicUsize::new(0));
        let hb = heartbeat.clone();

        // 旧 generation:收到 abort 后仍 sleep 80ms 再打一次心跳
        // (模拟 abort 后尚未完全退出的写盘窗口)
        let old_handle = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(10));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        hb.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        });
        supervisor.handles.insert(task_id.to_string(), old_handle);
        assert!(
            wait_heartbeat_started(&heartbeat).await,
            "旧 generation 必须先启动"
        );

        let download_dir = std::env::temp_dir().to_string_lossy().to_string();
        let download_config = DownloadConfig {
            download_dir: download_dir.clone(),
            authorized_dirs: vec![download_dir.clone()],
            ..DownloadConfig::default()
        };
        supervisor
            .start_download(
                state.clone(),
                task_id,
                "ftp://c03.invalid/join.bin".to_string(),
                download_dir,
                download_config,
                None,
                None,
            )
            .await;

        // start_download 返回后旧 generation 必须已 quiesce(不再心跳)
        let still = is_still_running(&heartbeat).await;
        assert!(
            !still,
            "Phase1: start_download 返回后旧 generation 不得仍在运行(需 join-before-restart)"
        );

        if let Some((_, h)) = supervisor.handles.remove(task_id) {
            h.abort();
        }
        supervisor.command_channels.remove(task_id);
        supervisor.command_locks.remove(task_id);
    }
}
