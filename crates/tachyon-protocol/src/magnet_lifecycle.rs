//! 磁力 Session acquisition 与 cleanup 的最小协调接缝。
//!
//! 此模块持有每个 info hash 的 scope 状态与外部 adapter，并负责真实 librqbit
//! provenance、lease、重试和 deadline 的 lane-owned 执行。
//!
//! 生产 MagnetProtocol 路径通过 coordinator 使用这组 seam；cleanup capability 仍只
//! 能从同一 protocol/coordinator 的窄方法取回，不在本模块或上层创建第二个 registry。
//! 下方仅对确实未被常规构建直接调用的具体 seam 项抑制 dead code lint，以保持常规
//! 构建的 clippy 零警告。

use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};

use futures::FutureExt;
use librqbit::{AddTorrent, AddTorrentOptions, ManagedTorrent};
use tokio::sync::{oneshot, watch};
use tokio::task::AbortHandle;
use tokio::time::Instant;

/// 外部 Session acquisition 边界的最小测试/生产 adapter。
///
/// adapter 实现不得保留 coordinator、lane 或 cleanup action。lane 持有 adapter，
/// 而 worker 仅以 `Weak<Lane>` 回写完成结果，避免 `Lane -> JoinHandle -> task -> Lane`
/// 强引用环。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) trait AcquisitionAdapter: Send + Sync {
    fn add(
        &self,
        request: AcquisitionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AcquisitionRegistration, AdapterError>> + Send + '_>>;

    fn retire(
        &self,
        registration: AcquisitionRegistration,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<(), AdapterError>> + Send + '_>>;
}

/// 一次 acquisition 所属的 BT v1 info hash 请求。
///
/// 请求是一次性 owned 值：真实 librqbit lane 携带 owned torrent descriptor，
/// 不把 Session 或任何外部 owner 放进 request。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct AcquisitionRequest {
    info_hash: [u8; 20],
    librqbit_add: Option<(AddTorrent<'static>, AddTorrentOptions)>,
}

impl AcquisitionRequest {
    #[cfg(test)]
    pub(crate) const fn for_test(info_hash: [u8; 20]) -> Self {
        Self {
            info_hash,
            librqbit_add: None,
        }
    }

    pub(crate) fn for_librqbit(
        info_hash: [u8; 20],
        add: AddTorrent<'static>,
        options: AddTorrentOptions,
    ) -> Self {
        Self {
            info_hash,
            librqbit_add: Some((add, options)),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_librqbit_test(
        info_hash: [u8; 20],
        add: AddTorrent<'static>,
        options: AddTorrentOptions,
    ) -> Self {
        Self::for_librqbit(info_hash, add, options)
    }

    pub(crate) fn into_librqbit_parts(
        self,
    ) -> Option<([u8; 20], AddTorrent<'static>, AddTorrentOptions)> {
        let Self {
            info_hash,
            librqbit_add,
        } = self;
        librqbit_add.map(|(add, options)| (info_hash, add, options))
    }
}

/// adapter 确认已注册的资源令牌及其 BT provenance。
///
/// `managed_torrent` 是真实 Session 返回的 exact Arc；cleanup 只能凭这个
/// provenance 退役，不能退化为仅凭可碰撞身份的删除。
#[derive(Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct AcquisitionRegistration {
    info_hash: [u8; 20],
    torrent_id: usize,
    managed_torrent: Option<Arc<ManagedTorrent>>,
}

impl std::fmt::Debug for AcquisitionRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcquisitionRegistration")
            .field("info_hash", &self.info_hash)
            .field("torrent_id", &self.torrent_id)
            .field("has_managed_torrent", &self.managed_torrent.is_some())
            .finish()
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl AcquisitionRegistration {
    #[cfg(test)]
    pub(crate) const fn for_test() -> Self {
        Self {
            info_hash: [0; 20],
            torrent_id: 0,
            managed_torrent: None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn for_test_with_provenance(info_hash: [u8; 20], torrent_id: usize) -> Self {
        Self {
            info_hash,
            torrent_id,
            managed_torrent: None,
        }
    }

    pub(crate) const fn info_hash(&self) -> [u8; 20] {
        self.info_hash
    }

    pub(crate) const fn torrent_id(&self) -> usize {
        self.torrent_id
    }

    pub(crate) fn managed_torrent(&self) -> Option<Arc<ManagedTorrent>> {
        self.managed_torrent.clone()
    }

    pub(crate) fn from_managed_torrent(
        info_hash: [u8; 20],
        torrent_id: usize,
        managed_torrent: Arc<ManagedTorrent>,
    ) -> Self {
        Self {
            info_hash,
            torrent_id,
            managed_torrent: Some(managed_torrent),
        }
    }
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum WorkerExitKind {
    Acquisition,
    Retirement,
}

/// worker future 的同步退出 guard。
///
/// guard 只持有 lane 的弱引用，因此 Tokio task 不会与 lane 形成强引用环；它的
/// `Drop` 路径不跨 await，只在 worker 尚未提交共享结果时补交终止结果。
struct WorkerExitGuard {
    lane: Weak<Lane>,
    kind: WorkerExitKind,
    armed: bool,
}

impl WorkerExitGuard {
    fn acquisition(lane: Weak<Lane>) -> Self {
        Self {
            lane,
            kind: WorkerExitKind::Acquisition,
            armed: true,
        }
    }

    fn retirement(lane: Weak<Lane>) -> Self {
        Self {
            lane,
            kind: WorkerExitKind::Retirement,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let Some(lane) = self.lane.upgrade() else {
            return;
        };
        match self.kind {
            WorkerExitKind::Acquisition => {
                lane.record_acquisition_result(Err(AcquisitionError::WorkerTerminated));
            }
            WorkerExitKind::Retirement => {
                lane.record_retirement_result(Err(BtCleanupError::WorkerTerminated));
            }
        }
    }
}

/// 外部 adapter 边界的失败分类。
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum AdapterError {
    #[allow(dead_code)]
    Failed,
    Timeout,
}

/// acquisition 返回的最小错误分类。
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum AcquisitionError {
    Adapter(AdapterError),
    ScopeRetiring,
    WorkerTerminated,
}

/// cleanup 已收敛的结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum BtCleanupOutcome {
    Converged,
    NoLease,
}

/// cleanup 外部退役失败。
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum BtCleanupError {
    Adapter(AdapterError),
    WorkerTerminated,
    Timeout,
}

impl std::fmt::Display for BtCleanupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Adapter(error) => write!(formatter, "cleanup adapter failed: {error:?}"),
            Self::WorkerTerminated => formatter.write_str("cleanup worker terminated"),
            Self::Timeout => formatter.write_str("cleanup timed out"),
        }
    }
}

impl std::error::Error for BtCleanupError {}

/// 可观察的 scope 状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum ScopeState {
    Acquiring,
    Retiring,
}

/// 由 lane 持有的 acquisition worker record。
///
/// `result` 由 task 在退出前同步提交；finished handle 只在提交结果后由 lane reaper
/// 移除。这样 caller waiter 的 abort 不会影响外部 adapter future。
#[cfg_attr(not(test), allow(dead_code))]
struct AcquisitionWorker {
    abort_handle: AbortHandle,
    result: Option<Result<AcquisitionRegistration, AcquisitionError>>,
    joined: bool,
}

/// 由 lane 持有的单一 retirement worker record。
///
/// `registration` 仍保存在 `LaneState`，worker 仅取得副本。因此外部 retire 失败时，
/// lane 仍持有资源并缓存失败结果，而不是把资源遗失给被取消的 waiter。
#[cfg_attr(not(test), allow(dead_code))]
struct RetirementWorker {
    abort_handle: AbortHandle,
    result: Option<Result<BtCleanupOutcome, BtCleanupError>>,
    joined: bool,
}

/// 每个 info hash 的唯一 coordinator lane。
#[cfg_attr(not(test), allow(dead_code))]
struct LaneRegistry {
    lanes: Mutex<HashMap<[u8; 20], Arc<Lane>>>,
}

#[cfg_attr(not(test), allow(dead_code))]
struct Lane {
    adapter: Arc<dyn AcquisitionAdapter>,
    registry: Weak<LaneRegistry>,
    info_hash: [u8; 20],
    state: Mutex<LaneState>,
    updates: watch::Sender<ScopeState>,
    #[cfg(test)]
    acquisition_waiter_armed: watch::Sender<bool>,
    #[cfg(test)]
    retirement_waiter_armed: watch::Sender<bool>,
    #[cfg(test)]
    worker_exit_holds: Mutex<HashMap<WorkerExitKind, WorkerExitHold>>,
}

#[cfg(test)]
struct WorkerExitHold {
    entered: watch::Sender<bool>,
    release: watch::Sender<bool>,
}

struct WaiterGuard {
    lane: Arc<Lane>,
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        self.lane.release_waiter();
    }
}

#[cfg_attr(not(test), allow(dead_code))]
struct LaneState {
    scope_state: ScopeState,
    active_waiters: usize,
    acquisition: Option<AcquisitionWorker>,
    registration: Option<AcquisitionRegistration>,
    cleanup_deadline: Option<Instant>,
    cleanup_result: Option<Result<BtCleanupOutcome, BtCleanupError>>,
    cleanup_retrying: bool,
    /// 是否已经登记过 request-level 的后台 cleanup。
    ///
    /// `scope_state` 只表示 lane 已进入 retiring，不能区分普通 cleanup waiter
    /// 与 detached background request；该标记专门保证 request 入口 single-flight。
    background_cleanup_requested: bool,
    retirement: Option<RetirementWorker>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl Lane {
    fn new(
        adapter: Arc<dyn AcquisitionAdapter>,
        info_hash: [u8; 20],
        registry: Weak<LaneRegistry>,
    ) -> Arc<Self> {
        let (updates, _) = watch::channel(ScopeState::Acquiring);
        #[cfg(test)]
        let (acquisition_waiter_armed, _) = watch::channel(false);
        #[cfg(test)]
        let (retirement_waiter_armed, _) = watch::channel(false);

        Arc::new(Self {
            adapter,
            registry,
            info_hash,
            state: Mutex::new(LaneState {
                scope_state: ScopeState::Acquiring,
                active_waiters: 0,
                acquisition: None,
                registration: None,
                cleanup_deadline: None,
                cleanup_result: None,
                cleanup_retrying: false,
                background_cleanup_requested: false,
                retirement: None,
            }),
            updates,
            #[cfg(test)]
            acquisition_waiter_armed,
            #[cfg(test)]
            retirement_waiter_armed,
            #[cfg(test)]
            worker_exit_holds: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(test)]
    fn mark_acquisition_waiter_armed(&self) {
        self.acquisition_waiter_armed.send_replace(true);
    }

    #[cfg(test)]
    fn mark_retirement_waiter_armed(&self) {
        self.retirement_waiter_armed.send_replace(true);
    }

    fn notify(&self, state: ScopeState) {
        self.updates.send_replace(state);
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, LaneState> {
        self.state.lock().expect("magnet lifecycle lane lock")
    }

    /// 成功收敛且不再持有任何活跃资源时，从 registry 摘除当前 lane。
    ///
    /// worker 的 result 提交早于 task 退出，故这里必须等待 join monitor 标记
    /// `joined`，不能把“已经有结果”误当成 worker 已经退出。waiter 的 RAII guard
    /// 也必须先离开，避免旧 action 仍在返回路径上时摘除 lane。
    fn try_reclaim_if_vacant(self: &Arc<Self>) {
        let vacant = {
            let state = self.lock_state();
            state.scope_state == ScopeState::Retiring
                && matches!(
                    state.cleanup_result.as_ref(),
                    Some(Ok(BtCleanupOutcome::Converged | BtCleanupOutcome::NoLease))
                )
                && !state.cleanup_retrying
                && state.registration.is_none()
                && state.active_waiters == 0
                && state
                    .acquisition
                    .as_ref()
                    .is_none_or(|worker| worker.joined)
                && state.retirement.as_ref().is_none_or(|worker| worker.joined)
        };
        if !vacant {
            return;
        }

        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        let mut lanes = registry.lanes.lock().expect("magnet lifecycle lanes lock");
        if lanes
            .get(&self.info_hash)
            .is_some_and(|lane| Arc::ptr_eq(lane, self))
        {
            lanes.remove(&self.info_hash);
        }
    }

    fn workers_joined(state: &LaneState) -> bool {
        state
            .acquisition
            .as_ref()
            .is_none_or(|worker| worker.joined)
            && state.retirement.as_ref().is_none_or(|worker| worker.joined)
    }

    fn cleanup_result_is_returnable(state: &LaneState) -> bool {
        match state.cleanup_result.as_ref() {
            Some(Ok(_)) => Self::workers_joined(state),
            Some(Err(_)) => true,
            None => false,
        }
    }

    fn acquire_waiter(self: &Arc<Self>) -> WaiterGuard {
        self.lock_state().active_waiters += 1;
        WaiterGuard {
            lane: Arc::clone(self),
        }
    }

    fn release_waiter(self: &Arc<Self>) {
        {
            let mut state = self.lock_state();
            state.active_waiters = state.active_waiters.saturating_sub(1);
        }
        self.try_reclaim_if_vacant();
    }

    #[cfg(test)]
    fn hold_worker_exit_for_test(&self, kind: WorkerExitKind) {
        let (entered, _) = watch::channel(false);
        let (release, _) = watch::channel(false);
        self.worker_exit_holds
            .lock()
            .expect("magnet lifecycle worker exit hold lock")
            .insert(kind, WorkerExitHold { entered, release });
    }

    #[cfg(test)]
    async fn wait_for_worker_exit_hold(&self, kind: WorkerExitKind) {
        let mut entered = {
            let holds = self
                .worker_exit_holds
                .lock()
                .expect("magnet lifecycle worker exit hold lock");
            holds
                .get(&kind)
                .expect("worker exit hold must be armed")
                .entered
                .subscribe()
        };
        if !*entered.borrow() {
            entered
                .changed()
                .await
                .expect("worker exit hold sender must outlive the lane");
        }
    }

    #[cfg(test)]
    async fn wait_at_worker_exit_hold(&self, kind: WorkerExitKind) {
        let Some(mut release) = ({
            let holds = self
                .worker_exit_holds
                .lock()
                .expect("magnet lifecycle worker exit hold lock");
            holds.get(&kind).map(|hold| {
                hold.entered.send_replace(true);
                hold.release.subscribe()
            })
        }) else {
            return;
        };
        if !*release.borrow() {
            let _ = release.changed().await;
        }
    }

    #[cfg(test)]
    fn release_worker_exit_hold(&self, kind: WorkerExitKind) {
        let holds = self
            .worker_exit_holds
            .lock()
            .expect("magnet lifecycle worker exit hold lock");
        if let Some(hold) = holds.get(&kind) {
            hold.release.send_replace(true);
        }
    }

    fn record_join(self: &Arc<Self>, kind: WorkerExitKind) {
        let state = {
            let mut lane_state = self.lock_state();
            match kind {
                WorkerExitKind::Acquisition => {
                    if let Some(worker) = lane_state.acquisition.as_mut() {
                        worker.joined = true;
                        if worker.result.is_none() {
                            worker.result = Some(Err(AcquisitionError::WorkerTerminated));
                        }
                    }
                }
                WorkerExitKind::Retirement => {
                    if let Some(worker) = lane_state.retirement.as_mut() {
                        worker.joined = true;
                        if worker.result.is_none() {
                            worker.result = Some(Err(BtCleanupError::WorkerTerminated));
                            if lane_state.cleanup_result.is_none() || lane_state.cleanup_retrying {
                                lane_state.cleanup_result =
                                    Some(Err(BtCleanupError::WorkerTerminated));
                                lane_state.cleanup_retrying = false;
                            }
                        }
                    }
                }
            }
            Self::complete_no_lease_if_ready(&mut lane_state);
            lane_state.scope_state
        };
        self.notify(state);
        self.try_reclaim_if_vacant();
    }

    /// 在 adapter 调用前将 acquisition task 记录在 lane；一次性 gate 在 record
    /// 已写入并释放 mutex 后才允许 task 调用 adapter。
    fn start_acquisition_worker(
        lane: &Arc<Self>,
        request: AcquisitionRequest,
    ) -> Result<(), AcquisitionError> {
        let startup = {
            let mut state = lane.lock_state();
            if state.scope_state == ScopeState::Retiring || state.acquisition.is_some() {
                return Err(AcquisitionError::ScopeRetiring);
            }

            let (startup_tx, startup_rx) = oneshot::channel();
            let adapter = Arc::clone(&lane.adapter);
            let completion_lane = Arc::downgrade(lane);
            let attempt_lane = Arc::downgrade(lane);
            let exit_guard = WorkerExitGuard::acquisition(Weak::clone(&completion_lane));
            let worker = tokio::spawn(async move {
                Self::run_acquisition_worker(
                    adapter,
                    request,
                    attempt_lane,
                    completion_lane.clone(),
                    startup_rx,
                    exit_guard,
                )
                .await;
            });
            let abort_handle = worker.abort_handle();
            let monitor_lane = Arc::downgrade(lane);
            tokio::spawn(async move {
                let _ = worker.await;
                if let Some(lane) = monitor_lane.upgrade() {
                    lane.record_join(WorkerExitKind::Acquisition);
                }
            });
            state.acquisition = Some(AcquisitionWorker {
                abort_handle,
                result: None,
                joined: false,
            });
            startup_tx
        };

        // 不在 mutex 锁内 await；send 发生在 worker record 入 lane 之后。
        let _ = startup.send(());
        Ok(())
    }

    /// acquisition task 的 adapter await 期间只保留 adapter、request、gate 和
    /// Weak lane。只有 await 已结束后才升级 Weak 并提交状态。
    async fn run_acquisition_worker(
        adapter: Arc<dyn AcquisitionAdapter>,
        request: AcquisitionRequest,
        attempt_lane: Weak<Self>,
        completion_lane: Weak<Self>,
        startup: oneshot::Receiver<()>,
        mut exit_guard: WorkerExitGuard,
    ) {
        let result = match AssertUnwindSafe(async move {
            if startup.await.is_err() {
                return Err(AcquisitionError::WorkerTerminated);
            }

            let adapter_result = adapter.add(request).await;
            let Some(lane) = attempt_lane.upgrade() else {
                return Err(AcquisitionError::WorkerTerminated);
            };
            Self::apply_acquisition_completion(&lane, adapter_result)
        })
        .catch_unwind()
        .await
        {
            Ok(result) => result,
            Err(_) => Err(AcquisitionError::WorkerTerminated),
        };

        if let Some(lane) = completion_lane.upgrade() {
            lane.record_acquisition_result(result);
            exit_guard.disarm();
            #[cfg(test)]
            lane.wait_at_worker_exit_hold(WorkerExitKind::Acquisition)
                .await;
        } else {
            exit_guard.disarm();
        }
    }

    /// 在 adapter `add` 已完成后提交 registration。Retiring 中的 late
    /// registration 保留在 lane，而不是发布给 caller；随后由同一 lane 的
    /// retirement worker single-flight 收敛。
    fn apply_acquisition_completion(
        lane: &Arc<Self>,
        adapter_result: Result<AcquisitionRegistration, AdapterError>,
    ) -> Result<AcquisitionRegistration, AcquisitionError> {
        match adapter_result {
            Ok(registration) => {
                let retiring = {
                    let mut state = lane.lock_state();
                    state.registration = Some(registration.clone());
                    state.scope_state == ScopeState::Retiring
                };

                if retiring {
                    Self::ensure_retirement_worker(lane);
                    Err(AcquisitionError::ScopeRetiring)
                } else {
                    Ok(registration)
                }
            }
            Err(error) => {
                let state = lane.lock_state();
                if state.scope_state == ScopeState::Retiring {
                    Err(AcquisitionError::ScopeRetiring)
                } else {
                    Err(AcquisitionError::Adapter(error))
                }
            }
        }
    }

    fn record_acquisition_result(
        self: &Arc<Self>,
        result: Result<AcquisitionRegistration, AcquisitionError>,
    ) {
        let state = {
            let mut lane_state = self.lock_state();
            let worker = lane_state
                .acquisition
                .as_mut()
                .expect("acquisition completion must retain its worker record");
            worker.result = Some(result);
            Self::complete_no_lease_if_ready(&mut lane_state);
            lane_state.scope_state
        };
        self.notify(state);
        self.try_reclaim_if_vacant();
    }

    /// 启动或复用唯一 retirement task。资源仍在 `LaneState.registration` 中；task
    /// 只捕获其副本，以确保 waiter abort、adapter failure 或 task panic 均不会 orphan
    /// 该注册资源。
    fn ensure_retirement_worker(lane: &Arc<Self>) {
        let startup = {
            let mut state = lane.lock_state();
            if state.scope_state != ScopeState::Retiring
                || state.cleanup_result.is_some()
                || state.retirement.is_some()
            {
                return;
            }

            let Some(registration) = state.registration.clone() else {
                return;
            };
            let deadline = state
                .cleanup_deadline
                .expect("retiring scope must retain its first cleanup deadline");
            let (startup_tx, startup_rx) = oneshot::channel();
            let adapter = Arc::clone(&lane.adapter);
            let completion_lane = Arc::downgrade(lane);
            let exit_guard = WorkerExitGuard::retirement(Weak::clone(&completion_lane));
            let worker = tokio::spawn(async move {
                Self::run_retirement_worker(
                    adapter,
                    registration,
                    deadline,
                    completion_lane.clone(),
                    startup_rx,
                    exit_guard,
                )
                .await;
            });
            let abort_handle = worker.abort_handle();
            let monitor_lane = Arc::downgrade(lane);
            tokio::spawn(async move {
                let _ = worker.await;
                if let Some(lane) = monitor_lane.upgrade() {
                    lane.record_join(WorkerExitKind::Retirement);
                }
            });
            state.retirement = Some(RetirementWorker {
                abort_handle,
                result: None,
                joined: false,
            });
            startup_tx
        };

        // 一次性 gate 保证 retirement record 已入 lane，才会进入 adapter.retire。
        let _ = startup.send(());
    }

    /// retirement task 的 adapter await 期间不持有强 lane 引用。完成后才升级 Weak
    /// 并缓存共享 cleanup result。
    async fn run_retirement_worker(
        adapter: Arc<dyn AcquisitionAdapter>,
        registration: AcquisitionRegistration,
        deadline: Instant,
        completion_lane: Weak<Self>,
        startup: oneshot::Receiver<()>,
        mut exit_guard: WorkerExitGuard,
    ) {
        let result = match AssertUnwindSafe(async move {
            if startup.await.is_err() {
                return Err(BtCleanupError::WorkerTerminated);
            }
            adapter
                .retire(registration, deadline)
                .await
                .map(|()| BtCleanupOutcome::Converged)
                .map_err(|error| match error {
                    AdapterError::Timeout => BtCleanupError::Timeout,
                    error => BtCleanupError::Adapter(error),
                })
        })
        .catch_unwind()
        .await
        {
            Ok(result) => result,
            Err(_) => Err(BtCleanupError::WorkerTerminated),
        };

        if let Some(lane) = completion_lane.upgrade() {
            lane.record_retirement_result(result);
            exit_guard.disarm();
            #[cfg(test)]
            lane.wait_at_worker_exit_hold(WorkerExitKind::Retirement)
                .await;
        } else {
            exit_guard.disarm();
        }
    }

    fn record_retirement_result(
        self: &Arc<Self>,
        result: Result<BtCleanupOutcome, BtCleanupError>,
    ) {
        let state = {
            let mut lane_state = self.lock_state();
            let worker = lane_state
                .retirement
                .as_mut()
                .expect("retirement completion must retain its worker record");
            worker.result = Some(result.clone());
            if lane_state.cleanup_retrying || lane_state.cleanup_result.is_none() {
                if result.is_ok() {
                    lane_state.registration = None;
                }
                lane_state.cleanup_result = Some(result);
                lane_state.cleanup_retrying = false;
            }
            lane_state.scope_state
        };
        self.notify(state);
        self.try_reclaim_if_vacant();
    }

    fn promote_retirement_result_if_retrying(state: &mut LaneState) {
        if !state.cleanup_retrying {
            return;
        }

        let Some(result) = state
            .retirement
            .as_ref()
            .and_then(|worker| worker.result.clone())
        else {
            return;
        };

        if result.is_ok() {
            state.registration = None;
        }
        state.cleanup_result = Some(result);
        state.cleanup_retrying = false;
    }

    fn record_cleanup_timeout(
        &self,
        observed_deadline: Instant,
    ) -> Option<Result<BtCleanupOutcome, BtCleanupError>> {
        let (state, result, notify) = {
            let mut lane_state = self.lock_state();
            if lane_state.cleanup_deadline != Some(observed_deadline) {
                return None;
            }

            let (result, notify) = match lane_state.cleanup_result.clone() {
                None => {
                    let result = Err(BtCleanupError::Timeout);
                    lane_state.cleanup_result = Some(result.clone());
                    lane_state.cleanup_retrying = false;
                    (result, true)
                }
                Some(result) if result.is_ok() && !Self::workers_joined(&lane_state) => {
                    let result = Err(BtCleanupError::Timeout);
                    lane_state.cleanup_result = Some(result.clone());
                    lane_state.cleanup_retrying = false;
                    (result, true)
                }
                Some(result) => (result, false),
            };
            (lane_state.scope_state, result, notify)
        };
        if notify {
            self.notify(state);
        }
        Some(result)
    }

    /// 外部 task 若在 catch boundary 之外异常结束，不能让 lane 永远把它视为活跃。
    /// 正常 worker 会先提交 result，再在此处由完成后的 handle 被 reaped；未提交 result
    /// 的 finished handle 诚实地缓存为 worker termination error。
    fn reap_finished_workers(self: &Arc<Self>) {
        // Join monitors are authoritative. This remains a defensive pass for a monitor
        // that observed a terminated task before it could publish its result; it never
        // replaces an already cached timeout or successful result.
        let state = {
            let mut lane_state = self.lock_state();
            let mut changed = false;
            if let Some(worker) = lane_state.acquisition.as_mut()
                && worker.joined
                && worker.result.is_none()
            {
                worker.result = Some(Err(AcquisitionError::WorkerTerminated));
                changed = true;
            }
            let terminated_retirement = lane_state
                .retirement
                .as_ref()
                .is_some_and(|worker| worker.joined && worker.result.is_none());
            if terminated_retirement {
                if let Some(worker) = lane_state.retirement.as_mut() {
                    worker.result = Some(Err(BtCleanupError::WorkerTerminated));
                }
                if lane_state.cleanup_result.is_none() || lane_state.cleanup_retrying {
                    lane_state.cleanup_result = Some(Err(BtCleanupError::WorkerTerminated));
                    lane_state.cleanup_retrying = false;
                    changed = true;
                }
            }
            let prior_cleanup = lane_state.cleanup_result.clone();
            Self::promote_retirement_result_if_retrying(&mut lane_state);
            Self::complete_no_lease_if_ready(&mut lane_state);
            changed |= prior_cleanup != lane_state.cleanup_result;
            changed.then_some(lane_state.scope_state)
        };

        if let Some(state) = state {
            self.notify(state);
        }
        self.try_reclaim_if_vacant();
    }

    fn complete_no_lease_if_ready(state: &mut LaneState) {
        let acquisition_result = state
            .acquisition
            .as_ref()
            .and_then(|worker| worker.result.as_ref());

        // acquisition 失败不能证明从未创建 lease。尤其是被 abort 的 worker 可能已有
        // finished record，却从未发布 registration。保留已有 Timeout，但让等待中的
        // cleanup（以及显式 retry）以 WorkerTerminated fail-closed，而不是伪造 NoLease。
        if acquisition_result.is_some_and(Result::is_err)
            && state.scope_state == ScopeState::Retiring
            && state.registration.is_none()
            && (state.cleanup_result.is_none() || state.cleanup_retrying)
        {
            state.cleanup_result = Some(Err(BtCleanupError::WorkerTerminated));
            state.cleanup_retrying = false;
            return;
        }

        let acquisition_succeeded = state
            .acquisition
            .as_ref()
            .is_none_or(|worker| worker.result.as_ref().is_some_and(Result::is_ok));
        if state.scope_state == ScopeState::Retiring
            && state.cleanup_result.is_none()
            && state.registration.is_none()
            && state.retirement.is_none()
            && acquisition_succeeded
        {
            state.cleanup_result = Some(Ok(BtCleanupOutcome::NoLease));
        }
    }
}

/// 每个 Session 的 acquisition/cleanup 唯一协调者。
///
/// 这是跨 crate 传递的 opaque capability；acquisition adapter 与生命周期操作
/// 仍保持在协议 crate 内部，engine 只能持有并共享其 `Arc`。
#[cfg_attr(not(test), allow(dead_code))]
pub struct MagnetSessionCoordinator {
    adapter: Arc<dyn AcquisitionAdapter>,
    registry: Arc<LaneRegistry>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum TestWorkerKind {
    Acquisition,
    Retirement,
}

#[cfg(test)]
impl From<TestWorkerKind> for WorkerExitKind {
    fn from(kind: TestWorkerKind) -> Self {
        match kind {
            TestWorkerKind::Acquisition => Self::Acquisition,
            TestWorkerKind::Retirement => Self::Retirement,
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl MagnetSessionCoordinator {
    /// 从协议层内部 adapter 创建 coordinator。
    pub(crate) fn from_adapter(adapter: Arc<dyn AcquisitionAdapter>) -> Self {
        Self {
            adapter,
            registry: Arc::new(LaneRegistry {
                lanes: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// 从当前 registry 取回同一 info hash lane 的 cleanup capability。
    ///
    /// 只返回 registry 中已经存在的 lane；不会创建新 scope、coordinator，或通过
    /// Session 句柄推断资源。返回的 action 与 lane 共享同一 exact registration。
    pub(crate) fn cleanup_action_for(&self, info_hash: [u8; 20]) -> Option<BtCleanupAction> {
        let lane = self
            .registry
            .lanes
            .lock()
            .expect("magnet lifecycle lanes lock")
            .get(&info_hash)
            .cloned()?;
        Some(BtCleanupAction { lane })
    }

    /// 当前 micro-slice 仅由 deterministic adapter 创建 coordinator。
    #[cfg(test)]
    pub(crate) fn with_test_adapter<A>(adapter: Arc<A>) -> Self
    where
        A: AcquisitionAdapter + 'static,
    {
        Self::from_adapter(adapter)
    }

    #[cfg(test)]
    pub(crate) fn with_adapter_for_test<A>(adapter: Arc<A>) -> Self
    where
        A: AcquisitionAdapter + 'static,
    {
        Self::with_test_adapter(adapter)
    }

    /// 在启动 adapter add future 前同步登记 acquisition scope。
    pub(crate) fn begin_acquire(
        &self,
        request: AcquisitionRequest,
    ) -> Result<MagnetAcquisition, AcquisitionError> {
        let lane = {
            let mut lanes = self
                .registry
                .lanes
                .lock()
                .expect("magnet lifecycle lanes lock");
            if lanes.contains_key(&request.info_hash) {
                return Err(AcquisitionError::ScopeRetiring);
            }

            let lane = Lane::new(
                Arc::clone(&self.adapter),
                request.info_hash,
                Arc::downgrade(&self.registry),
            );
            lanes.insert(request.info_hash, Arc::clone(&lane));
            lane
        };

        Ok(MagnetAcquisition { lane, request })
    }

    /// 等待指定 info hash scope 进入目标状态；用于 deterministic 测试同步。
    pub(crate) async fn wait_for_state(&self, info_hash: [u8; 20], target: ScopeState) {
        let lane = {
            let lanes = self
                .registry
                .lanes
                .lock()
                .expect("magnet lifecycle lanes lock");
            lanes
                .get(&info_hash)
                .cloned()
                .expect("magnet lifecycle scope must exist")
        };
        let mut updates = lane.updates.subscribe();

        loop {
            if *updates.borrow() == target {
                return;
            }
            updates
                .changed()
                .await
                .expect("magnet lifecycle lane sender must outlive its scope");
        }
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_worker_waiter_for_test(
        &self,
        info_hash: [u8; 20],
        kind: TestWorkerKind,
    ) {
        let lane = {
            let lanes = self
                .registry
                .lanes
                .lock()
                .expect("magnet lifecycle lanes lock");
            lanes
                .get(&info_hash)
                .cloned()
                .expect("magnet lifecycle scope must exist")
        };
        let mut armed = match kind {
            TestWorkerKind::Acquisition => lane.acquisition_waiter_armed.subscribe(),
            TestWorkerKind::Retirement => lane.retirement_waiter_armed.subscribe(),
        };

        if *armed.borrow() {
            return;
        }
        armed
            .changed()
            .await
            .expect("magnet lifecycle waiter armed sender must outlive its scope");
    }

    #[cfg(test)]
    pub(crate) fn has_registration_for_test(&self, info_hash: [u8; 20]) -> bool {
        let lanes = self
            .registry
            .lanes
            .lock()
            .expect("magnet lifecycle lanes lock");
        lanes
            .get(&info_hash)
            .is_some_and(|lane| lane.lock_state().registration.is_some())
    }

    #[cfg(test)]
    pub(crate) fn abort_worker_for_test(&self, info_hash: [u8; 20], kind: TestWorkerKind) {
        let lane = {
            let lanes = self
                .registry
                .lanes
                .lock()
                .expect("magnet lifecycle lanes lock");
            lanes
                .get(&info_hash)
                .cloned()
                .expect("magnet lifecycle scope must exist")
        };
        let state = lane.lock_state();
        match kind {
            TestWorkerKind::Acquisition => state
                .acquisition
                .as_ref()
                .expect("acquisition worker must be lane-owned")
                .abort_handle
                .abort(),
            TestWorkerKind::Retirement => state
                .retirement
                .as_ref()
                .expect("retirement worker must be lane-owned")
                .abort_handle
                .abort(),
        }
    }

    #[cfg(test)]
    pub(crate) fn hold_worker_exit_for_test(&self, info_hash: [u8; 20], kind: TestWorkerKind) {
        let lane = self.lane_for_test(info_hash);
        lane.hold_worker_exit_for_test(kind.into());
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_worker_exit_hold_for_test(
        &self,
        info_hash: [u8; 20],
        kind: TestWorkerKind,
    ) {
        let lane = self.lane_for_test(info_hash);
        lane.wait_for_worker_exit_hold(kind.into()).await;
    }

    #[cfg(test)]
    pub(crate) fn release_worker_exit_for_test(&self, info_hash: [u8; 20], kind: TestWorkerKind) {
        let lane = self.lane_for_test(info_hash);
        lane.release_worker_exit_hold(kind.into());
    }

    #[cfg(test)]
    fn lane_for_test(&self, info_hash: [u8; 20]) -> Arc<Lane> {
        self.registry
            .lanes
            .lock()
            .expect("magnet lifecycle lanes lock")
            .get(&info_hash)
            .cloned()
            .expect("magnet lifecycle scope must exist")
    }
}

/// 已同步注册、尚未开始或正在进行 acquisition 的 scope。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct MagnetAcquisition {
    lane: Arc<Lane>,
    request: AcquisitionRequest,
}

#[cfg_attr(not(test), allow(dead_code))]
impl MagnetAcquisition {
    /// 提供与该 scope 绑定的可 clone cleanup capability。
    pub(crate) fn cleanup_action(&self) -> BtCleanupAction {
        BtCleanupAction {
            lane: Arc::clone(&self.lane),
        }
    }

    /// 同步启动并由 lane 托管 acquisition worker，返回只等待共享结果的 caller future。
    /// caller abort/drop 不会中止 adapter.add。
    pub(crate) fn start(
        self,
    ) -> Pin<Box<dyn Future<Output = Result<AcquisitionRegistration, AcquisitionError>> + Send>>
    {
        let lane = Arc::clone(&self.lane);
        let startup = Lane::start_acquisition_worker(&lane, self.request);

        Box::pin(async move {
            startup?;
            Self::wait_for_worker(lane).await
        })
    }

    /// 兼容便利 API；实际 adapter worker 仍由 lane 持有而非 caller future。
    pub(crate) async fn acquire(self) -> Result<AcquisitionRegistration, AcquisitionError> {
        self.start().await
    }

    async fn wait_for_worker(lane: Arc<Lane>) -> Result<AcquisitionRegistration, AcquisitionError> {
        let _waiter = lane.acquire_waiter();
        let mut updates = lane.updates.subscribe();
        loop {
            lane.reap_finished_workers();
            {
                let state = lane.lock_state();
                let worker = state
                    .acquisition
                    .as_ref()
                    .expect("started magnet acquisition must retain its lane worker");
                if let Some(result) = &worker.result {
                    return result.clone();
                }
            }
            #[cfg(test)]
            lane.mark_acquisition_waiter_armed();
            updates
                .changed()
                .await
                .expect("magnet lifecycle lane sender must outlive its scope");
        }
    }
}

/// 绑定 scope 的 opaque cleanup capability。
#[derive(Clone)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct BtCleanupAction {
    lane: Arc<Lane>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl BtCleanupAction {
    /// 在 lane mutex 下登记 request-level cleanup，尚不启动后台 waiter。
    ///
    /// protocol 需要先用这个原子 gate 摘除当前 binding，再启动 background waiter，
    /// 避免 cleanup 极快收敛并 reclaim lane 后 stale binding 又被新一代 acquisition
    /// 观察到。registry 校验保证持有的旧 action 不能在 lane 已 reclaim 后重新打开 scope。
    pub(crate) fn reserve_background_cleanup(&self) -> bool {
        let Some(registry) = self.lane.registry.upgrade() else {
            return false;
        };
        let mut state = self.lane.lock_state();
        if state.background_cleanup_requested
            || state.scope_state == ScopeState::Retiring
            || state.cleanup_result.is_some()
        {
            return false;
        }

        // try_reclaim_if_vacant 释放 lane state lock 后才获取 registry lock，因此这里
        // 可保持 state -> registry 顺序完成校验；该 lane 仍为 Acquiring，不能被 reclaim。
        let lanes = registry.lanes.lock().expect("magnet lifecycle lanes lock");
        if !lanes
            .get(&self.lane.info_hash)
            .is_some_and(|lane| Arc::ptr_eq(lane, &self.lane))
        {
            return false;
        }

        state.background_cleanup_requested = true;
        true
    }

    /// 启动已经由 [`Self::reserve_background_cleanup`] 登记的后台 waiter。
    pub(crate) fn spawn_background_cleanup(&self, deadline: Instant) {
        let action = self.clone();
        tokio::spawn(async move {
            if let Err(error) = action.cleanup_until(deadline).await {
                tracing::warn!(
                    stage = "background_cleanup",
                    error = ?error,
                    "磁力链接后台 cleanup 未收敛"
                );
            }
        });
    }

    /// 请求同一 lane 后台收敛 cleanup，不等待退役结果。
    ///
    /// 后台 task 只持有该 action 的 clone；唯一的 retirement worker、registration 与
    /// cleanup result 仍由 lane 持有。调用方不拥有 detached task，也不能通过丢弃
    /// action 取消 lane-owned cleanup。重复 request 不会启动新的 waiter。
    pub(crate) fn request_background_cleanup(&self, deadline: Instant) -> bool {
        if !self.reserve_background_cleanup() {
            return false;
        }
        self.spawn_background_cleanup(deadline);
        true
    }

    /// 关闭 scope，并等待 lane-owned acquisition/retirement worker 的共享结果。
    ///
    /// 首个 caller 的 deadline 只在 scope 首次转 Retiring 时写入，并由所有 waiter
    /// 共享执行；后续 waiter 既不重置该 deadline，也不重试已超时的 cleanup。
    pub(crate) async fn cleanup_until(
        &self,
        deadline: Instant,
    ) -> Result<BtCleanupOutcome, BtCleanupError> {
        self.cleanup_until_inner(deadline, false).await
    }

    /// 显式重试已经超时的 cleanup，复用原有 retirement worker，不创建新的退役尝试。
    pub(crate) async fn retry_cleanup_until(
        &self,
        deadline: Instant,
    ) -> Result<BtCleanupOutcome, BtCleanupError> {
        self.cleanup_until_inner(deadline, true).await
    }

    async fn cleanup_until_inner(
        &self,
        deadline: Instant,
        retry_on_timeout: bool,
    ) -> Result<BtCleanupOutcome, BtCleanupError> {
        let _waiter = self.lane.acquire_waiter();
        let mut updates = self.lane.updates.subscribe();

        loop {
            self.lane.reap_finished_workers();
            let (next_step, notify_state) = {
                let mut state = self.lane.lock_state();
                let mut retry_started = false;
                let mut immediate_result = None;

                if let Some(result) = state.cleanup_result.clone() {
                    if retry_on_timeout && matches!(result, Err(BtCleanupError::Timeout)) {
                        state.cleanup_result = None;
                        state.cleanup_retrying = true;
                        state.background_cleanup_requested = false;
                        state.cleanup_deadline = Some(deadline);
                        retry_started = true;
                    } else if !(matches!(result, Err(BtCleanupError::Timeout))
                        && state.cleanup_retrying)
                        && Lane::cleanup_result_is_returnable(&state)
                    {
                        immediate_result = Some(result);
                    }
                }

                if let Some(result) = immediate_result {
                    (CleanupStep::Return(result), None)
                } else {
                    Lane::promote_retirement_result_if_retrying(&mut state);
                    if let Some(result) = state.cleanup_result.clone()
                        && !(matches!(result, Err(BtCleanupError::Timeout))
                            && state.cleanup_retrying)
                        && Lane::cleanup_result_is_returnable(&state)
                    {
                        (CleanupStep::Return(result), None)
                    } else {
                        let entered_retiring = state.scope_state == ScopeState::Acquiring;
                        if entered_retiring {
                            state.scope_state = ScopeState::Retiring;
                            state.cleanup_deadline = Some(deadline);
                        }

                        let acquisition_pending = state
                            .acquisition
                            .as_ref()
                            .is_some_and(|worker| worker.result.is_none());
                        let success_waiting_for_join = state
                            .cleanup_result
                            .as_ref()
                            .is_some_and(|result| result.is_ok() && !Lane::workers_joined(&state));
                        let next_step =
                            if acquisition_pending
                                || state.retirement.is_some()
                                || success_waiting_for_join
                            {
                                let cleanup_deadline = state
                                    .cleanup_deadline
                                    .expect("retiring scope must retain its cleanup deadline");
                                CleanupStep::Wait { cleanup_deadline }
                            } else if state.registration.is_some() {
                                CleanupStep::StartRetirement
                            } else {
                                Lane::complete_no_lease_if_ready(&mut state);
                                CleanupStep::Return(state.cleanup_result.clone().expect(
                                    "closed scope without workers or registration is NoLease",
                                ))
                            };
                        let notify_state =
                            (entered_retiring || retry_started).then_some(state.scope_state);
                        (next_step, notify_state)
                    }
                }
            };

            if let Some(state) = notify_state {
                self.lane.notify(state);
            }
            match next_step {
                CleanupStep::StartRetirement => Lane::ensure_retirement_worker(&self.lane),
                CleanupStep::Return(result) => {
                    self.lane.try_reclaim_if_vacant();
                    return result;
                }
                CleanupStep::Wait { cleanup_deadline } => {
                    #[cfg(test)]
                    self.lane.mark_retirement_waiter_armed();
                    match tokio::time::timeout_at(cleanup_deadline, updates.changed()).await {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) => {
                            panic!("magnet lifecycle lane sender must outlive its scope")
                        }
                        Err(_) => {
                            if let Some(result) = self.lane.record_cleanup_timeout(cleanup_deadline)
                            {
                                return result;
                            }
                            // 观测到的 deadline 已被显式 retry 替换时，重新读取最新状态。
                            continue;
                        }
                    }
                }
            }
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
enum CleanupStep {
    StartRetirement,
    Return(Result<BtCleanupOutcome, BtCleanupError>),
    Wait { cleanup_deadline: Instant },
}
