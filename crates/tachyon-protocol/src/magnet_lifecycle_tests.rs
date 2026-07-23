//! Magnet session lifecycle 的严格 TDD RED 合同。
//!
//! 本模块只通过 lifecycle 的 crate-private adapter seam 驱动外部 Session 边界；
//! 不 mock coordinator 或其状态机规则。

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dashmap::DashMap;
use librqbit::api::TorrentIdOrHash::{Hash, Id};
use librqbit::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, Session, SessionOptions, create_torrent,
};
use tempfile::TempDir;
use tokio::sync::{Semaphore, oneshot};
use tokio::time::{Instant, timeout};

use tachyon_core::config::MagnetConfig;
use tachyon_core::traits::Protocol;

use crate::magnet_lifecycle::{
    AcquisitionAdapter, AcquisitionError, AcquisitionRegistration, AcquisitionRequest,
    AdapterError, BtCleanupError, BtCleanupOutcome, MagnetSessionCoordinator, ScopeState,
    TestWorkerKind,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(1);

/// 可控的外部 Session adapter：`add` 开始后保持 pending，直到测试显式交付注册结果。
///
/// 它刻意忽略 coordinator 发出的取消请求，模拟 cleanup 与 `add_torrent` 完成竞速时
/// librqbit 在取消后仍晚到返回一个已注册资源的边界行为。coordinator 必须负责收敛该资源。
struct DelayedRegistrationAdapter {
    acquisition_started: Mutex<Option<oneshot::Sender<()>>>,
    late_registration: Mutex<Option<oneshot::Receiver<AcquisitionRegistration>>>,
    retirement_started: Mutex<Option<oneshot::Sender<()>>>,
}

impl DelayedRegistrationAdapter {
    fn new() -> (
        Arc<Self>,
        oneshot::Receiver<()>,
        oneshot::Sender<AcquisitionRegistration>,
        oneshot::Receiver<()>,
    ) {
        let (acquisition_started_tx, acquisition_started_rx) = oneshot::channel();
        let (late_registration_tx, late_registration_rx) = oneshot::channel();
        let (retirement_started_tx, retirement_started_rx) = oneshot::channel();
        (
            Arc::new(Self {
                acquisition_started: Mutex::new(Some(acquisition_started_tx)),
                late_registration: Mutex::new(Some(late_registration_rx)),
                retirement_started: Mutex::new(Some(retirement_started_tx)),
            }),
            acquisition_started_rx,
            late_registration_tx,
            retirement_started_rx,
        )
    }
}

impl AcquisitionAdapter for DelayedRegistrationAdapter {
    fn add(
        &self,
        _request: AcquisitionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AcquisitionRegistration, AdapterError>> + Send + '_>>
    {
        let acquisition_started = self
            .acquisition_started
            .lock()
            .expect("adapter acquisition_started lock")
            .take()
            .expect("add must start exactly once");
        let late_registration = self
            .late_registration
            .lock()
            .expect("adapter late_registration lock")
            .take()
            .expect("add must wait for exactly one registration");

        Box::pin(async move {
            acquisition_started
                .send(())
                .expect("test must wait until add starts");
            Ok(late_registration
                .await
                .expect("test must release the late registration"))
        })
    }

    fn retire(
        &self,
        _registration: AcquisitionRegistration,
        _deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<(), AdapterError>> + Send + '_>> {
        let retirement_started = self
            .retirement_started
            .lock()
            .expect("adapter retirement_started lock")
            .take()
            .expect("late registration must be retired exactly once");

        Box::pin(async move {
            retirement_started
                .send(())
                .expect("test must wait for late registration retirement");
            Ok(())
        })
    }
}

/// 让 production probe 在 metadata/registration 后续阶段失败的 deterministic adapter。
///
/// `add` 已经返回 exact registration 后，probe 因 registration 没有 ManagedTorrent
/// 失败；`retire` 使用 semaphore 保持 lane 在 `Retiring`，使测试可以先观察同一 lane
/// 的状态与 cleanup 请求，再显式释放 retirement。没有真实 Session::get/delete 旁路。
struct ProbeFailureCleanupAdapter {
    registration: Mutex<Option<AcquisitionRegistration>>,
    acquisition_started: Mutex<Option<oneshot::Sender<()>>>,
    retirement_started: Mutex<Option<oneshot::Sender<()>>>,
    retirement_release: Arc<Semaphore>,
    retire_calls: AtomicUsize,
}

impl ProbeFailureCleanupAdapter {
    fn new(
        registration: AcquisitionRegistration,
    ) -> (Arc<Self>, oneshot::Receiver<()>, oneshot::Receiver<()>) {
        let (acquisition_started_tx, acquisition_started_rx) = oneshot::channel();
        let (retirement_started_tx, retirement_started_rx) = oneshot::channel();
        (
            Arc::new(Self {
                registration: Mutex::new(Some(registration)),
                acquisition_started: Mutex::new(Some(acquisition_started_tx)),
                retirement_started: Mutex::new(Some(retirement_started_tx)),
                retirement_release: Arc::new(Semaphore::new(0)),
                retire_calls: AtomicUsize::new(0),
            }),
            acquisition_started_rx,
            retirement_started_rx,
        )
    }

    fn release_retirement(&self) {
        self.retirement_release.add_permits(1);
    }

    fn retire_calls(&self) -> usize {
        self.retire_calls.load(Ordering::SeqCst)
    }
}

impl AcquisitionAdapter for ProbeFailureCleanupAdapter {
    fn add(
        &self,
        _request: AcquisitionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AcquisitionRegistration, AdapterError>> + Send + '_>>
    {
        let acquisition_started = self
            .acquisition_started
            .lock()
            .expect("probe failure acquisition_started lock")
            .take()
            .expect("probe add must start exactly once");
        let registration = self
            .registration
            .lock()
            .expect("probe failure registration lock")
            .take()
            .expect("probe add must return its exact registration once");
        Box::pin(async move {
            acquisition_started
                .send(())
                .expect("test must wait until probe add starts");
            Ok(registration)
        })
    }

    fn retire(
        &self,
        _registration: AcquisitionRegistration,
        _deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<(), AdapterError>> + Send + '_>> {
        assert_eq!(
            self.retire_calls.fetch_add(1, Ordering::SeqCst),
            0,
            "production probe failure must request one coordinator cleanup"
        );
        let retirement_started = self
            .retirement_started
            .lock()
            .expect("probe failure retirement_started lock")
            .take()
            .expect("probe failure retirement must start exactly once");
        let retirement_release = Arc::clone(&self.retirement_release);
        Box::pin(async move {
            retirement_started
                .send(())
                .expect("test must wait until probe cleanup starts");
            let _permit = retirement_release
                .acquire()
                .await
                .expect("test must retain the probe cleanup release semaphore");
            Ok(())
        })
    }
}

#[tokio::test]
async fn aborting_lane_owned_acquisition_worker_wakes_waiter_with_worker_terminated() {
    let (adapter, acquisition_started, _late_registration, _retirement_started) =
        DelayedRegistrationAdapter::new();
    let coordinator = MagnetSessionCoordinator::with_test_adapter(adapter);
    let info_hash = [0xA8; 20];

    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let waiter = tokio::spawn(acquisition.start());

    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("adapter add was not started")
        .expect("adapter dropped the acquisition-start signal");
    coordinator
        .wait_for_worker_waiter_for_test(info_hash, TestWorkerKind::Acquisition)
        .await;

    coordinator.abort_worker_for_test(info_hash, TestWorkerKind::Acquisition);

    let acquisition_error = timeout(TEST_TIMEOUT, waiter)
        .await
        .expect("acquisition waiter remained pending after its worker was aborted")
        .expect("acquisition waiter must not panic")
        .expect_err("aborting the lane-owned worker must terminate the waiter");
    assert_eq!(acquisition_error, AcquisitionError::WorkerTerminated);
}

#[tokio::test]
async fn aborting_acquisition_worker_does_not_report_no_lease_to_cleanup() {
    let (adapter, acquisition_started, _late_registration, _retirement_started) =
        DelayedRegistrationAdapter::new();
    let coordinator = MagnetSessionCoordinator::with_test_adapter(adapter);
    let info_hash = [0xAC; 20];

    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let cleanup = acquisition.cleanup_action();
    let acquisition_waiter = tokio::spawn(acquisition.start());

    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("adapter add was not started")
        .expect("adapter dropped the acquisition-start signal");

    let cleanup_waiter =
        tokio::spawn(async move { cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT).await });

    timeout(
        TEST_TIMEOUT,
        coordinator.wait_for_state(info_hash, ScopeState::Retiring),
    )
    .await
    .expect("cleanup action did not close the acquiring scope");
    coordinator
        .wait_for_worker_waiter_for_test(info_hash, TestWorkerKind::Acquisition)
        .await;

    coordinator.abort_worker_for_test(info_hash, TestWorkerKind::Acquisition);

    let acquisition_error = timeout(TEST_TIMEOUT, acquisition_waiter)
        .await
        .expect("acquisition waiter remained pending after its worker was aborted")
        .expect("acquisition waiter must not panic")
        .expect_err("aborting the lane-owned worker must terminate the waiter");
    assert_eq!(acquisition_error, AcquisitionError::WorkerTerminated);

    let cleanup_result = timeout(TEST_TIMEOUT, cleanup_waiter)
        .await
        .expect("cleanup waiter remained pending after acquisition worker termination")
        .expect("cleanup waiter must not panic");
    assert_eq!(cleanup_result, Err(BtCleanupError::WorkerTerminated));
}

#[tokio::test]
async fn aborting_lane_owned_retirement_worker_wakes_cleanup_waiter_with_worker_terminated() {
    let (adapter, acquisition_started, registration, retirement_started) =
        BlockingRetirementAdapter::new();
    let coordinator = MagnetSessionCoordinator::with_test_adapter(Arc::clone(&adapter));
    let info_hash = [0xA9; 20];

    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let cleanup = acquisition.cleanup_action();
    let caller = tokio::spawn(acquisition.start());

    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("adapter add was not started")
        .expect("adapter dropped the acquisition-start signal");
    registration
        .send(AcquisitionRegistration::for_test())
        .expect("lane-owned worker must accept the registration");
    caller
        .await
        .expect("acquisition caller must not panic")
        .expect("registration must complete acquisition before cleanup");

    let waiter =
        tokio::spawn(async move { cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT).await });
    timeout(TEST_TIMEOUT, retirement_started)
        .await
        .expect("registered resource retirement did not start")
        .expect("adapter dropped the retirement-start signal");
    coordinator
        .wait_for_worker_waiter_for_test(info_hash, TestWorkerKind::Retirement)
        .await;

    coordinator.abort_worker_for_test(info_hash, TestWorkerKind::Retirement);

    let cleanup_error = timeout(TEST_TIMEOUT, waiter)
        .await
        .expect("cleanup waiter remained pending after its worker was aborted")
        .expect("cleanup waiter must not panic")
        .expect_err("aborting the lane-owned worker must terminate the waiter");
    assert_eq!(cleanup_error, BtCleanupError::WorkerTerminated);
}

#[tokio::test]
async fn cleanup_closes_acquiring_scope_and_reclaims_late_added_resource() {
    let (adapter, acquisition_started, late_registration, retirement_started) =
        DelayedRegistrationAdapter::new();
    let coordinator = MagnetSessionCoordinator::with_test_adapter(adapter);
    let info_hash = [0xA5; 20];

    // begin_acquire 必须在启动外部 add future 之前同步创建 scope，使 action 可先取得。
    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let cleanup = acquisition.cleanup_action();
    let worker = tokio::spawn(acquisition.acquire());

    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("adapter add was not started before cleanup")
        .expect("adapter dropped the acquisition-start signal");

    let cleanup_worker =
        tokio::spawn(async move { cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT).await });

    // 不靠 sleep 或轮询：观察 action 先到时对同一 scope 的原子状态转换。
    timeout(
        TEST_TIMEOUT,
        coordinator.wait_for_state(info_hash, ScopeState::Retiring),
    )
    .await
    .expect("cleanup action did not close the acquiring scope");

    // 模拟 add_torrent 在取消后才返回的已注册资源；它不得发布为可用 lease。
    late_registration
        .send(AcquisitionRegistration::for_test())
        .expect("coordinator worker must still track the pending add");

    timeout(TEST_TIMEOUT, retirement_started)
        .await
        .expect("late registration escaped cleanup")
        .expect("adapter dropped the retirement signal");

    let acquisition_error = worker
        .await
        .expect("acquisition worker must not panic")
        .expect_err("closed scope must reject a late registration");
    assert!(matches!(acquisition_error, AcquisitionError::ScopeRetiring));

    let cleanup_outcome = cleanup_worker
        .await
        .expect("cleanup worker must not panic")
        .expect("late registration retirement must converge");
    assert_eq!(cleanup_outcome, BtCleanupOutcome::Converged);
}

#[tokio::test]
async fn successful_cleanup_reclaims_lane_for_next_generation() {
    let (adapter, acquisition_started, registration, retirement_started) =
        BlockingRetirementAdapter::new();
    let coordinator = MagnetSessionCoordinator::with_test_adapter(Arc::clone(&adapter));
    let info_hash = [0xAD; 20];

    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let cleanup = acquisition.cleanup_action();
    let caller = tokio::spawn(acquisition.start());

    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("adapter add was not started")
        .expect("adapter dropped the acquisition-start signal");
    registration
        .send(AcquisitionRegistration::for_test())
        .expect("lane-owned worker must accept the registration");
    caller
        .await
        .expect("acquisition caller must not panic")
        .expect("registration must complete acquisition before cleanup");

    let cleanup_waiter =
        tokio::spawn(async move { cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT).await });
    timeout(TEST_TIMEOUT, retirement_started)
        .await
        .expect("registered resource retirement did not start")
        .expect("adapter dropped the retirement-start signal");
    adapter.release_retirement();

    let cleanup_outcome = timeout(TEST_TIMEOUT, cleanup_waiter)
        .await
        .expect("cleanup waiter did not return")
        .expect("cleanup waiter must not panic")
        .expect("successful cleanup must converge");
    assert_eq!(cleanup_outcome, BtCleanupOutcome::Converged);

    let next_acquisition = coordinator.begin_acquire(AcquisitionRequest::for_test(info_hash));
    assert!(
        next_acquisition.is_ok(),
        "successful cleanup must reclaim the lane for the next generation; got {:?}",
        next_acquisition.as_ref().err()
    );
}

#[tokio::test]
async fn lane_reclamation_waits_for_worker_exit_and_cleanup_waiter() {
    let (adapter, acquisition_started, registration, retirement_started) =
        BlockingRetirementAdapter::new();
    let coordinator = MagnetSessionCoordinator::with_test_adapter(Arc::clone(&adapter));
    let info_hash = [0xAE; 20];

    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let cleanup = acquisition.cleanup_action();
    let caller = tokio::spawn(acquisition.start());

    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("adapter add was not started")
        .expect("adapter dropped the acquisition-start signal");
    registration
        .send(AcquisitionRegistration::for_test())
        .expect("lane-owned worker must accept the registration");
    caller
        .await
        .expect("acquisition caller must not panic")
        .expect("registration must complete acquisition before cleanup");

    let mut cleanup_waiter = Box::pin(tokio::spawn(async move {
        cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT).await
    }));
    timeout(TEST_TIMEOUT, retirement_started)
        .await
        .expect("registered resource retirement did not start")
        .expect("adapter dropped the retirement-start signal");

    coordinator.hold_worker_exit_for_test(info_hash, TestWorkerKind::Retirement);
    adapter.release_retirement();

    let mut exit_hold = Box::pin(
        coordinator.wait_for_worker_exit_hold_for_test(info_hash, TestWorkerKind::Retirement),
    );
    tokio::select! {
        biased;
        result = &mut cleanup_waiter => panic!(
            "cleanup waiter returned before retirement worker exit gate: {:?}",
            result
        ),
        _ = &mut exit_hold => {}
    }

    let retiring_acquisition = coordinator.begin_acquire(AcquisitionRequest::for_test(info_hash));
    assert!(
        matches!(retiring_acquisition, Err(AcquisitionError::ScopeRetiring)),
        "lane must remain registered while the retirement worker exit gate is held"
    );

    coordinator.release_worker_exit_for_test(info_hash, TestWorkerKind::Retirement);

    let cleanup_outcome = timeout(TEST_TIMEOUT, &mut cleanup_waiter)
        .await
        .expect("cleanup waiter did not return after the retirement worker exited")
        .expect("cleanup waiter must not panic")
        .expect("successful cleanup must converge");
    assert_eq!(cleanup_outcome, BtCleanupOutcome::Converged);

    let next_acquisition = coordinator.begin_acquire(AcquisitionRequest::for_test(info_hash));
    assert!(
        next_acquisition.is_ok(),
        "cleanup waiter completion must reclaim the lane for the next generation"
    );
}

#[tokio::test]
async fn aborting_cleanup_waiter_does_not_orphan_registered_resource_retirement() {
    let (adapter, acquisition_started, registration, retirement_started) =
        BlockingRetirementAdapter::new();
    let coordinator = MagnetSessionCoordinator::with_test_adapter(Arc::clone(&adapter));
    let info_hash = [0xA7; 20];

    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let cleanup = acquisition.cleanup_action();
    let caller = tokio::spawn(acquisition.start());

    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("adapter add was not started")
        .expect("adapter dropped the acquisition-start signal");
    registration
        .send(AcquisitionRegistration::for_test())
        .expect("lane-owned worker must accept the registration");
    caller
        .await
        .expect("acquisition caller must not panic")
        .expect("registration must complete acquisition before cleanup");

    let first_cleanup = cleanup.clone();
    let first_waiter = tokio::spawn(async move {
        first_cleanup
            .cleanup_until(Instant::now() + TEST_TIMEOUT)
            .await
    });

    timeout(TEST_TIMEOUT, retirement_started)
        .await
        .expect("registered resource retirement did not start")
        .expect("adapter dropped the retirement-start signal");

    first_waiter.abort();
    let first_waiter_error = first_waiter
        .await
        .expect_err("aborted cleanup waiter must not complete");
    assert!(first_waiter_error.is_cancelled());

    let second_waiter =
        tokio::spawn(async move { cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT).await });
    adapter.release_retirement();

    let second_outcome = timeout(TEST_TIMEOUT, second_waiter)
        .await
        .expect("second cleanup action did not converge after the first waiter was aborted")
        .expect("second cleanup waiter must not panic")
        .expect("registered resource retirement must converge");
    assert_eq!(second_outcome, BtCleanupOutcome::Converged);
    assert_eq!(
        adapter.retire_calls(),
        1,
        "retirement must remain single-flight"
    );
}

/// 已正常注册资源的可控 adapter：首个 `retire` 开始后阻塞，直到测试持久释放它。
///
/// `retire` 只能进入一次。release 使用共享 semaphore，而不是被 abort waiter 持有的
/// oneshot receiver，确保取消第一个 cleanup waiter 不会吞掉测试的释放通道。
struct BlockingRetirementAdapter {
    acquisition_started: Mutex<Option<oneshot::Sender<()>>>,
    registration: Mutex<Option<oneshot::Receiver<AcquisitionRegistration>>>,
    retirement_started: Mutex<Option<oneshot::Sender<()>>>,
    retirement_release: Arc<Semaphore>,
    retire_calls: AtomicUsize,
}

impl BlockingRetirementAdapter {
    fn new() -> (
        Arc<Self>,
        oneshot::Receiver<()>,
        oneshot::Sender<AcquisitionRegistration>,
        oneshot::Receiver<()>,
    ) {
        let (acquisition_started_tx, acquisition_started_rx) = oneshot::channel();
        let (registration_tx, registration_rx) = oneshot::channel();
        let (retirement_started_tx, retirement_started_rx) = oneshot::channel();
        (
            Arc::new(Self {
                acquisition_started: Mutex::new(Some(acquisition_started_tx)),
                registration: Mutex::new(Some(registration_rx)),
                retirement_started: Mutex::new(Some(retirement_started_tx)),
                retirement_release: Arc::new(Semaphore::new(0)),
                retire_calls: AtomicUsize::new(0),
            }),
            acquisition_started_rx,
            registration_tx,
            retirement_started_rx,
        )
    }

    fn release_retirement(&self) {
        self.retirement_release.add_permits(1);
    }

    fn retire_calls(&self) -> usize {
        self.retire_calls.load(Ordering::SeqCst)
    }
}

impl AcquisitionAdapter for BlockingRetirementAdapter {
    fn add(
        &self,
        _request: AcquisitionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AcquisitionRegistration, AdapterError>> + Send + '_>>
    {
        let acquisition_started = self
            .acquisition_started
            .lock()
            .expect("adapter acquisition_started lock")
            .take()
            .expect("add must start exactly once");
        let registration = self
            .registration
            .lock()
            .expect("adapter registration lock")
            .take()
            .expect("add must wait for exactly one registration");

        Box::pin(async move {
            acquisition_started
                .send(())
                .expect("test must wait until add starts");
            Ok(registration
                .await
                .expect("test must deliver the registration"))
        })
    }

    fn retire(
        &self,
        _registration: AcquisitionRegistration,
        _deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<(), AdapterError>> + Send + '_>> {
        assert_eq!(
            self.retire_calls.fetch_add(1, Ordering::SeqCst),
            0,
            "registered resource retirement must be single-flight"
        );
        let retirement_started = self
            .retirement_started
            .lock()
            .expect("adapter retirement_started lock")
            .take()
            .expect("first retirement must publish its start");
        let retirement_release = Arc::clone(&self.retirement_release);

        Box::pin(async move {
            retirement_started
                .send(())
                .expect("test must wait until retirement starts");
            let _permit = retirement_release
                .acquire()
                .await
                .expect("test must retain the retirement release semaphore");
            Ok(())
        })
    }
}

#[tokio::test]
async fn cleanup_deadline_is_terminal_when_worker_join_misses_deadline() {
    let (adapter, acquisition_started, registration, retirement_started) =
        BlockingRetirementAdapter::new();
    let coordinator = MagnetSessionCoordinator::with_test_adapter(Arc::clone(&adapter));
    let info_hash = [0xAF; 20];

    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let cleanup = acquisition.cleanup_action();
    let caller = tokio::spawn(acquisition.start());

    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("adapter add was not started")
        .expect("adapter dropped the acquisition-start signal");
    registration
        .send(AcquisitionRegistration::for_test())
        .expect("lane-owned worker must accept the registration");
    caller
        .await
        .expect("acquisition caller must not panic")
        .expect("registration must complete acquisition before cleanup");

    let deadline = Instant::now() + Duration::from_millis(50);
    let cleanup_waiter_action = cleanup.clone();
    let mut cleanup_waiter = Box::pin(tokio::spawn(async move {
        cleanup_waiter_action.cleanup_until(deadline).await
    }));
    timeout(TEST_TIMEOUT, retirement_started)
        .await
        .expect("registered resource retirement did not start")
        .expect("adapter dropped the retirement-start signal");

    coordinator.hold_worker_exit_for_test(info_hash, TestWorkerKind::Retirement);
    adapter.release_retirement();

    let mut exit_hold = Box::pin(
        coordinator.wait_for_worker_exit_hold_for_test(info_hash, TestWorkerKind::Retirement),
    );
    tokio::select! {
        biased;
        result = &mut cleanup_waiter => panic!(
            "cleanup waiter returned before retirement worker exit gate: {:?}",
            result
        ),
        _ = &mut exit_hold => {}
    }

    tokio::time::sleep_until(deadline + Duration::from_millis(20)).await;

    let cleanup_result = match timeout(TEST_TIMEOUT, &mut cleanup_waiter).await {
        Ok(join_result) => {
            let result = join_result.expect("cleanup waiter must not panic");
            coordinator.release_worker_exit_for_test(info_hash, TestWorkerKind::Retirement);
            result
        }
        Err(_) => {
            coordinator.release_worker_exit_for_test(info_hash, TestWorkerKind::Retirement);
            timeout(TEST_TIMEOUT, &mut cleanup_waiter)
                .await
                .expect("cleanup waiter remained pending after exit gate release")
                .expect("cleanup waiter must not panic after exit gate release")
        }
    };
    assert_eq!(cleanup_result, Err(BtCleanupError::Timeout));

    let ordinary_result = timeout(
        TEST_TIMEOUT,
        cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT),
    )
    .await
    .expect("ordinary cleanup waiter did not return");
    assert_eq!(ordinary_result, Err(BtCleanupError::Timeout));
    assert_eq!(adapter.retire_calls(), 1);
}

#[tokio::test]
async fn cleanup_deadline_returns_timeout_and_preserves_single_flight_resource() {
    let (adapter, acquisition_started, registration, retirement_started) =
        BlockingRetirementAdapter::new();
    let coordinator = MagnetSessionCoordinator::with_test_adapter(Arc::clone(&adapter));
    let info_hash = [0xAA; 20];

    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let cleanup = acquisition.cleanup_action();
    let caller = tokio::spawn(acquisition.start());

    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("adapter add was not started")
        .expect("adapter dropped the acquisition-start signal");
    registration
        .send(AcquisitionRegistration::for_test())
        .expect("lane-owned worker must accept the registration");
    caller
        .await
        .expect("acquisition caller must not panic")
        .expect("registration must complete acquisition before cleanup");

    let first_cleanup = cleanup.clone();
    let first_waiter = tokio::spawn(async move {
        first_cleanup
            .cleanup_until(Instant::now() + Duration::from_millis(50))
            .await
    });
    timeout(TEST_TIMEOUT, retirement_started)
        .await
        .expect("registered resource retirement did not start")
        .expect("adapter dropped the retirement-start signal");

    let first_result = timeout(TEST_TIMEOUT, first_waiter)
        .await
        .expect("cleanup waiter did not return by its deadline")
        .expect("cleanup waiter must not panic");
    assert_eq!(first_result, Err(BtCleanupError::Timeout));
    assert_eq!(
        adapter.retire_calls(),
        1,
        "the timeout must not start another retirement attempt"
    );

    adapter.release_retirement();
    let second_waiter =
        tokio::spawn(async move { cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT).await });
    let second_result = timeout(TEST_TIMEOUT, second_waiter)
        .await
        .expect("second cleanup waiter did not return")
        .expect("second cleanup waiter must not panic");
    assert_eq!(
        second_result,
        Err(BtCleanupError::Timeout),
        "releasing the original retirement must not turn a timed-out attempt into Converged"
    );
    assert_eq!(
        adapter.retire_calls(),
        1,
        "subsequent cleanup waiters must reuse the single retirement attempt"
    );
}

#[tokio::test]
async fn explicit_retry_reuses_timed_out_retirement_worker_and_converges() {
    let (adapter, acquisition_started, registration, retirement_started) =
        BlockingRetirementAdapter::new();
    let coordinator = MagnetSessionCoordinator::with_test_adapter(Arc::clone(&adapter));
    let info_hash = [0xAB; 20];

    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let cleanup = acquisition.cleanup_action();
    let caller = tokio::spawn(acquisition.start());

    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("adapter add was not started")
        .expect("adapter dropped the acquisition-start signal");
    registration
        .send(AcquisitionRegistration::for_test())
        .expect("lane-owned worker must accept the registration");
    caller
        .await
        .expect("acquisition caller must not panic")
        .expect("registration must complete acquisition before cleanup");

    let first_cleanup = cleanup.clone();
    let first_waiter = tokio::spawn(async move {
        first_cleanup
            .cleanup_until(Instant::now() + Duration::from_millis(50))
            .await
    });
    timeout(TEST_TIMEOUT, retirement_started)
        .await
        .expect("registered resource retirement did not start")
        .expect("adapter dropped the retirement-start signal");

    let first_result = timeout(TEST_TIMEOUT, first_waiter)
        .await
        .expect("cleanup waiter did not return by its deadline")
        .expect("cleanup waiter must not panic");
    assert_eq!(first_result, Err(BtCleanupError::Timeout));
    assert_eq!(adapter.retire_calls(), 1);

    let retry_cleanup = cleanup.clone();
    let retry_waiter = tokio::spawn(async move {
        retry_cleanup
            .retry_cleanup_until(Instant::now() + TEST_TIMEOUT)
            .await
    });
    coordinator
        .wait_for_worker_waiter_for_test(info_hash, TestWorkerKind::Retirement)
        .await;

    adapter.release_retirement();

    let retry_result = timeout(TEST_TIMEOUT, retry_waiter)
        .await
        .expect("explicit cleanup retry did not return")
        .expect("explicit cleanup retry must not panic")
        .expect("explicit cleanup retry must converge");
    assert_eq!(retry_result, BtCleanupOutcome::Converged);

    let ordinary_result = timeout(
        TEST_TIMEOUT,
        cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT),
    )
    .await
    .expect("ordinary cleanup waiter did not return")
    .expect("ordinary cleanup waiter must not fail");
    assert_eq!(ordinary_result, BtCleanupOutcome::Converged);
    assert_eq!(adapter.retire_calls(), 1);
}

#[tokio::test]
async fn aborting_acquisition_waiter_does_not_orphan_late_registration_cleanup() {
    let (adapter, acquisition_started, late_registration, retirement_started) =
        DelayedRegistrationAdapter::new();
    let coordinator = MagnetSessionCoordinator::with_test_adapter(adapter);
    let info_hash = [0xA6; 20];

    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let cleanup = acquisition.cleanup_action();
    let caller = tokio::spawn(acquisition.start());

    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("adapter add was not started before caller abort")
        .expect("adapter dropped the acquisition-start signal");

    caller.abort();
    let caller_error = caller
        .await
        .expect_err("aborted acquisition waiter must not complete");
    assert!(caller_error.is_cancelled());

    let cleanup_worker =
        tokio::spawn(async move { cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT).await });

    timeout(
        TEST_TIMEOUT,
        coordinator.wait_for_state(info_hash, ScopeState::Retiring),
    )
    .await
    .expect("cleanup action did not close the acquiring scope");

    late_registration
        .send(AcquisitionRegistration::for_test())
        .expect("lane-owned worker must still track the pending add");

    timeout(TEST_TIMEOUT, retirement_started)
        .await
        .expect("late registration escaped cleanup after caller abort")
        .expect("adapter dropped the retirement signal");

    let cleanup_outcome = cleanup_worker
        .await
        .expect("cleanup worker must not panic")
        .expect("late registration retirement must converge");
    assert_eq!(cleanup_outcome, BtCleanupOutcome::Converged);
}

/// 只记录 cleanup 收到的身份元组，不构造或伪造 librqbit 的私有 handle。
struct ProvenanceRecordingAdapter {
    registration: Mutex<Option<AcquisitionRegistration>>,
    retired_identity: Mutex<Option<([u8; 20], usize)>>,
}

impl ProvenanceRecordingAdapter {
    fn new(registration: AcquisitionRegistration) -> Arc<Self> {
        Arc::new(Self {
            registration: Mutex::new(Some(registration)),
            retired_identity: Mutex::new(None),
        })
    }

    fn retired_identity(&self) -> Option<([u8; 20], usize)> {
        self.retired_identity
            .lock()
            .expect("adapter retired_identity lock")
            .take()
    }
}

impl AcquisitionAdapter for ProvenanceRecordingAdapter {
    fn add(
        &self,
        _request: AcquisitionRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AcquisitionRegistration, AdapterError>> + Send + '_>>
    {
        let registration = self
            .registration
            .lock()
            .expect("adapter registration lock")
            .take()
            .expect("add must return exactly one registration");
        Box::pin(async move { Ok(registration) })
    }

    fn retire(
        &self,
        registration: AcquisitionRegistration,
        _deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<(), AdapterError>> + Send + '_>> {
        let identity = (registration.info_hash(), registration.torrent_id());
        *self
            .retired_identity
            .lock()
            .expect("adapter retired_identity lock") = Some(identity);
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn registration_preserves_info_hash_and_torrent_identity() {
    let info_hash = [0xB1; 20];
    let torrent_id = 41_usize;
    let registration = AcquisitionRegistration::for_test_with_provenance(info_hash, torrent_id);
    assert_eq!(registration.info_hash(), info_hash);
    assert_eq!(registration.torrent_id(), torrent_id);

    let adapter = ProvenanceRecordingAdapter::new(registration);
    let coordinator = MagnetSessionCoordinator::with_test_adapter(Arc::clone(&adapter));
    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let cleanup = acquisition.cleanup_action();

    acquisition
        .start()
        .await
        .expect("provenance registration must complete acquisition");
    let cleanup_outcome = cleanup
        .cleanup_until(Instant::now() + TEST_TIMEOUT)
        .await
        .expect("cleanup must not fail");
    assert_eq!(cleanup_outcome, BtCleanupOutcome::Converged);
    assert_eq!(
        adapter.retired_identity(),
        Some((info_hash, torrent_id)),
        "cleanup adapter must receive the original acquisition identity"
    );
}

struct RealLibrqbitAddedFixture {
    _source: TempDir,
    target: TempDir,
    session: Arc<Session>,
    adapter: Arc<crate::magnet::LibrqbitAcquisitionAdapter>,
    info_hash: [u8; 20],
    torrent_bytes: bytes::Bytes,
}

impl RealLibrqbitAddedFixture {
    fn acquisition_request(&self) -> AcquisitionRequest {
        AcquisitionRequest::for_librqbit_test(
            self.info_hash,
            AddTorrent::from_bytes(self.torrent_bytes.clone()),
            AddTorrentOptions {
                paused: false,
                output_folder: Some(self.target.path().to_string_lossy().into_owned()),
                overwrite: true,
                disable_trackers: true,
                ..Default::default()
            },
        )
    }
}

async fn real_librqbit_added_fixture()
-> Result<RealLibrqbitAddedFixture, Box<dyn std::error::Error>> {
    let source = TempDir::new()?;
    let target = TempDir::new()?;
    let payload = (0..96u8).collect::<Vec<_>>();
    let source_payload = source.path().join("payload.bin");
    let target_payload = target.path().join("payload.bin");
    std::fs::write(&source_payload, &payload)?;
    // Session 的输出目录与 torrent 的来源目录隔离，但仍预置相同内容，
    // 使 librqbit 在禁用网络后可以通过 initial check 完成 Added。
    std::fs::write(&target_payload, &payload)?;

    let torrent = create_torrent(
        &source_payload,
        CreateTorrentOptions {
            name: None,
            piece_length: Some(16 * 1024),
        },
    )
    .await?;
    let torrent_bytes = torrent.as_bytes()?;
    let info_hash = torrent.info_hash().0;

    let session = Session::new_with_opts(
        PathBuf::from(target.path()),
        SessionOptions {
            disable_dht: true,
            persistence: None,
            enable_upnp_port_forwarding: false,
            ..Default::default()
        },
    )
    .await?;
    let adapter = Arc::new(crate::magnet::LibrqbitAcquisitionAdapter::new(Arc::clone(
        &session,
    )));

    Ok(RealLibrqbitAddedFixture {
        _source: source,
        target,
        session,
        adapter,
        info_hash,
        torrent_bytes,
    })
}

#[tokio::test]
async fn real_librqbit_retire_with_expired_deadline_returns_timeout()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = real_librqbit_added_fixture().await?;
    let registration =
        AcquisitionAdapter::add(fixture.adapter.as_ref(), fixture.acquisition_request())
            .await
            .expect("real Added acquisition must return a registration");

    let result = AcquisitionAdapter::retire(
        fixture.adapter.as_ref(),
        registration,
        Instant::now() - Duration::from_millis(1),
    )
    .await;

    assert_eq!(result, Err(AdapterError::Timeout));
    Ok(())
}

#[tokio::test]
async fn real_librqbit_added_registration_retains_exact_handle_and_retires_by_id()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = real_librqbit_added_fixture().await?;
    let info_hash = fixture.info_hash;
    let session = Arc::clone(&fixture.session);
    let adapter = Arc::clone(&fixture.adapter);
    let coordinator = MagnetSessionCoordinator::with_adapter_for_test(Arc::clone(&adapter));
    let acquisition = coordinator
        .begin_acquire(fixture.acquisition_request())
        .expect("test acquisition scope must be created");
    let cleanup = acquisition.cleanup_action();

    acquisition
        .start()
        .await
        .expect("real librqbit acquisition must complete");

    let registration = adapter
        .last_registration_for_test()
        .expect("Added must record its registration for observation");
    let handle = registration
        .managed_torrent()
        .expect("Added registration must retain its ManagedTorrent handle");
    let info_hash_id = handle.info_hash();
    assert_eq!(info_hash_id.0, info_hash);
    let by_id = session
        .get(Id(handle.id()))
        .expect("Added torrent must be discoverable by id");
    assert_eq!(handle.id(), by_id.id());
    assert!(
        Arc::ptr_eq(&handle, &by_id),
        "registration must retain the exact Session handle"
    );
    let by_hash = session
        .get(Hash(info_hash_id))
        .expect("Added torrent must be discoverable by hash");
    assert!(Arc::ptr_eq(&handle, &by_hash));
    assert_eq!(handle.info_hash(), info_hash_id);

    let cleanup_outcome = timeout(
        TEST_TIMEOUT,
        cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT),
    )
    .await
    .expect("cleanup must finish before its test deadline")
    .expect("cleanup must converge");
    assert_eq!(cleanup_outcome, BtCleanupOutcome::Converged);
    assert!(session.get(Id(handle.id())).is_none());
    assert!(session.get(Hash(info_hash_id)).is_none());

    Ok(())
}

/// production acquisition helper 必须接收一次性 absolute deadline，而不是 Duration。
///
/// 当前 production helper 仍是私有且接收 `metadata_timeout: Duration`，因此这是预期的
/// compile RED。该调用把同一个 `Instant` 作为 add + initialization 流程的唯一预算边界；
/// 生产实现不得在 acquisition 返回后重新计算 `now + metadata_timeout`。
#[tokio::test]
async fn production_probe_uses_one_absolute_metadata_deadline() {
    let (adapter, _acquisition_started, _retirement_started) = ProbeFailureCleanupAdapter::new(
        AcquisitionRegistration::for_test_with_provenance([0xC1; 20], 73),
    );
    let coordinator = Arc::new(MagnetSessionCoordinator::with_test_adapter(adapter));
    let source = TempDir::new().expect("probe source tempdir");
    let magnet_url = format!("magnet:?xt=urn:btih:{}", hex::encode([0xC1; 20]));
    let deadline = Instant::now() + Duration::from_secs(2);

    // 预期窄 API：production probe 的 acquisition helper 暴露同一个绝对 deadline。
    // 若仍按 Duration 接收 metadata budget，或未提供这个 deterministic seam，此处必须
    // compile RED；deadline 是单一 Instant，不允许 helper 在 add 后重算第二个预算。
    let _ = crate::magnet::acquire_magnet_for_probe_with_deadline(
        &coordinator,
        &magnet_url,
        source.path(),
        None,
        Vec::new(),
        None,
        None,
        false,
        deadline,
    )
    .await;
}

/// production probe 在 registration 后失败时，必须启动同一 coordinator lane 的 tracked
/// nonblocking cleanup；probe 应立即返回原始错误，而不是等待外部 retirement 收敛。
///
/// 当前 production `MagnetProtocol::probe` 无法把 deterministic adapter 的 exact
/// registration 注入到“acquisition 已成功、后续 init/metadata 失败”的阶段；因此该测试
/// 引用预期的 `probe_with_coordinator_for_test` 窄 API，compile RED 即表示生产接缝缺失。
/// adapter 先返回 exact registration，再让 registration 缺失 ManagedTorrent，使失败发生在
/// acquisition 之后；retire 的 semaphore 保证可以先观察 Retiring 与 tracked cleanup request。
#[tokio::test]
async fn production_probe_failure_starts_tracked_nonblocking_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
    let info_hash = [0xC1; 20];
    let (adapter, acquisition_started, retirement_started) = ProbeFailureCleanupAdapter::new(
        AcquisitionRegistration::for_test_with_provenance(info_hash, 73),
    );
    let coordinator = Arc::new(MagnetSessionCoordinator::with_test_adapter(Arc::clone(
        &adapter,
    )));
    let root = TempDir::new()?;
    let session = Session::new_with_opts(
        root.path().to_path_buf(),
        SessionOptions {
            disable_dht: true,
            persistence: None,
            enable_upnp_port_forwarding: false,
            ..Default::default()
        },
    )
    .await?;
    let magnet_url = format!("magnet:?xt=urn:btih:{}", hex::encode(info_hash));
    let protocol = Arc::new(
        crate::magnet::MagnetProtocol::new(
            Arc::clone(&session),
            MagnetConfig {
                metadata_timeout_secs: 2,
                enable_dht: false,
                enable_upnp: false,
                trackers: Vec::new(),
                ..MagnetConfig::default()
            },
            root.path().to_path_buf(),
            Arc::new(DashMap::new()),
        )
        .with_session_coordinator(Arc::clone(&coordinator)),
    );

    let probe_url = magnet_url.clone();
    let probe_protocol = Arc::clone(&protocol);
    let probe = tokio::spawn(async move {
        probe_protocol
            .probe_with_coordinator_for_test(&probe_url)
            .await
    });

    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("production probe did not start coordinator acquisition")
        .expect("probe acquisition start signal was dropped");
    timeout(
        TEST_TIMEOUT,
        coordinator.wait_for_state(info_hash, ScopeState::Retiring),
    )
    .await
    .expect("probe failure did not close the same coordinator lane");
    timeout(TEST_TIMEOUT, retirement_started)
        .await
        .expect("probe failure did not request coordinator cleanup")
        .expect("probe cleanup start signal was dropped");

    // protocol 已移入 probe task；从同一 coordinator lane 取得 action，后续用它 join
    // background cleanup 的 single-flight 结果，而不是依赖 detached task 的 sleep/poll。
    let cleanup = protocol
        .cleanup_action_for(&magnet_url)
        .expect("probe failure must retain the same coordinator cleanup action");

    // retirement semaphore 尚未 release：probe 必须先返回 metadata/init 错误。若 production
    // 仍同步 await action.cleanup_until，这里会在 bounded timeout 后 RED。
    let probe_result = timeout(TEST_TIMEOUT, probe)
        .await
        .expect("probe failure must return before retirement is released")
        .expect("production probe task must not panic");
    assert!(
        probe_result.is_err(),
        "invalid metadata must fail the probe"
    );
    assert_eq!(
        adapter.retire_calls(),
        1,
        "probe failure must start exactly one coordinator retirement"
    );

    adapter.release_retirement();
    let cleanup_outcome = timeout(
        TEST_TIMEOUT,
        cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT),
    )
    .await
    .expect("tracked probe cleanup remained pending after retirement release")
    .expect("tracked probe cleanup must converge");
    assert_eq!(cleanup_outcome, BtCleanupOutcome::Converged);
    assert!(
        !coordinator.has_registration_for_test(info_hash),
        "coordinator must clear the exact registration after cleanup"
    );
    assert!(
        coordinator
            .begin_acquire(AcquisitionRequest::for_test(info_hash))
            .is_ok(),
        "a converged probe failure must reclaim the coordinator lane"
    );
    Ok(())
}

/// 后续 owner 请求 cleanup 时必须只登记后台 action，不得同步等待外部 Session retirement。
///
/// 当前 protocol 尚未提供 `request_background_cleanup_for`；因此该测试的 compile RED
/// 锁定新增 request API。retirement 使用 deterministic adapter，不把真实 Session delete
/// 当作后台 request 的生产证明。
#[tokio::test]
async fn protocol_background_cleanup_request_is_nonblocking_and_tracked()
-> Result<(), Box<dyn std::error::Error>> {
    let info_hash = [0xC2; 20];
    let (adapter, acquisition_started, retirement_started) = ProbeFailureCleanupAdapter::new(
        AcquisitionRegistration::for_test_with_provenance(info_hash, 74),
    );
    let coordinator = Arc::new(MagnetSessionCoordinator::with_test_adapter(Arc::clone(
        &adapter,
    )));
    let acquisition = coordinator
        .begin_acquire(AcquisitionRequest::for_test(info_hash))
        .expect("test acquisition scope must be created");
    let registration = timeout(TEST_TIMEOUT, acquisition.start())
        .await
        .expect("acquisition must finish before its test deadline")
        .expect("deterministic acquisition must return its registration");
    timeout(TEST_TIMEOUT, acquisition_started)
        .await
        .expect("adapter acquisition did not start")
        .expect("adapter dropped the acquisition-start signal");
    assert_eq!(registration.info_hash(), info_hash);

    let root = TempDir::new()?;
    let session = Session::new_with_opts(
        root.path().to_path_buf(),
        SessionOptions {
            disable_dht: true,
            persistence: None,
            enable_upnp_port_forwarding: false,
            ..Default::default()
        },
    )
    .await?;
    let magnet_url = format!("magnet:?xt=urn:btih:{}", hex::encode(info_hash));
    let protocol = crate::magnet::MagnetProtocol::new(
        Arc::clone(&session),
        MagnetConfig {
            metadata_timeout_secs: 2,
            enable_dht: false,
            enable_upnp: false,
            trackers: Vec::new(),
            ..MagnetConfig::default()
        },
        root.path().to_path_buf(),
        Arc::new(DashMap::new()),
    )
    .with_session_coordinator(Arc::clone(&coordinator));

    // 该调用必须在 retirement semaphore 仍被持有时立即返回；后台 worker 才能随后
    // 通过 retirement_started 被观察到。若 request API 同步等待退役，此处将直接卡住。
    assert!(
        protocol.request_background_cleanup_for(&magnet_url),
        "existing coordinator lane must accept the background cleanup request"
    );
    assert!(
        !protocol.request_background_cleanup_for(&magnet_url),
        "a second request for the retiring lane must not spawn another background waiter"
    );
    timeout(TEST_TIMEOUT, retirement_started)
        .await
        .expect("background cleanup worker did not start retirement")
        .expect("adapter dropped the retirement-start signal");

    adapter.release_retirement();
    let cleanup = protocol
        .cleanup_action_for(&magnet_url)
        .expect("background request must retain the same coordinator cleanup action");
    let cleanup_outcome = timeout(
        TEST_TIMEOUT,
        cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT),
    )
    .await
    .expect("tracked background cleanup remained pending")
    .expect("tracked background cleanup must converge");
    assert_eq!(cleanup_outcome, BtCleanupOutcome::Converged);
    assert!(
        !coordinator.has_registration_for_test(info_hash),
        "background cleanup must clear the exact coordinator registration"
    );
    assert!(
        coordinator
            .begin_acquire(AcquisitionRequest::for_test(info_hash))
            .is_ok(),
        "converged background cleanup must reclaim the lane for a new generation"
    );

    assert!(
        !protocol.request_background_cleanup_for("not-a-magnet"),
        "invalid cleanup request must fail closed"
    );
    let unknown_url = format!("magnet:?xt=urn:btih:{}", hex::encode([0xC3; 20]));
    assert!(
        !protocol.request_background_cleanup_for(&unknown_url),
        "cleanup request without an existing coordinator lane must fail closed"
    );
    Ok(())
}

/// Slice 3A 的首个真实 production probe RED：cache miss 的 Added 必须由 coordinator 持有。
///
/// 该测试故意不预置 client torrent/cache，并通过 loopback seeder 提供 BEP 9 metadata。
/// 成功 probe 后，后续 owner 必须能从同一 protocol/coordinator 取得 action，并由
/// coordinator 按 exact provenance 完成 cleanup；本测试不声称 engine/App 已经接线。
#[tokio::test]
async fn production_probe_registers_cache_miss_added_with_session_coordinator()
-> Result<(), Box<dyn std::error::Error>> {
    let seeder_root = TempDir::new()?;
    let client_root = TempDir::new()?;
    let payload = (0..96u8).collect::<Vec<_>>();
    let payload_path = seeder_root.path().join("payload.bin");
    std::fs::write(&payload_path, &payload)?;

    let torrent = create_torrent(
        &payload_path,
        CreateTorrentOptions {
            name: None,
            piece_length: Some(16 * 1024),
        },
    )
    .await?;
    let torrent_bytes = torrent.as_bytes()?;
    let info_hash = torrent.info_hash();
    let info_hash_bytes = info_hash.0;
    let magnet_url = format!("magnet:?xt=urn:btih:{}", hex::encode(info_hash_bytes));

    // Seeder 只使用临时目录和固定的独立 listener 范围；不启用 DHT、持久化或 UPnP。
    let seeder_session = Session::new_with_opts(
        seeder_root.path().to_path_buf(),
        SessionOptions {
            disable_dht: true,
            persistence: None,
            listen_port_range: Some(45_000..45_100),
            enable_upnp_port_forwarding: false,
            ..Default::default()
        },
    )
    .await?;
    let seeder_handle = seeder_session
        .add_torrent(
            AddTorrent::from_bytes(torrent_bytes.clone()),
            Some(AddTorrentOptions {
                paused: false,
                output_folder: Some(seeder_root.path().to_string_lossy().into_owned()),
                overwrite: true,
                disable_trackers: true,
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .ok_or_else(|| std::io::Error::other("seeder torrent must be Added"))?;
    timeout(Duration::from_secs(5), seeder_handle.wait_until_completed()).await??;
    let seeder_port = seeder_session
        .tcp_listen_port()
        .ok_or_else(|| std::io::Error::other("seeder must expose a TCP listen port"))?;

    // Client 不预置 torrent 或 cache；probe 必须真实走 cache miss + initial peer metadata resolve。
    let client_session = Session::new_with_opts(
        client_root.path().to_path_buf(),
        SessionOptions {
            disable_dht: true,
            persistence: None,
            enable_upnp_port_forwarding: false,
            ..Default::default()
        },
    )
    .await?;
    let config = MagnetConfig {
        metadata_timeout_secs: 5,
        enable_dht: false,
        enable_upnp: false,
        trackers: Vec::new(),
        peer_addrs: vec![format!("127.0.0.1:{seeder_port}")],
        // S-01 默认拒绝 loopback; loopback seeder fixture 必须显式 opt-in。
        allow_private_peers: true,
        ..MagnetConfig::default()
    };

    let coordinator = crate::magnet::new_librqbit_session_coordinator(Arc::clone(&client_session));
    let handle_cache = Arc::new(DashMap::new());
    let protocol = crate::magnet::MagnetProtocol::new(
        Arc::clone(&client_session),
        config,
        client_root.path().to_path_buf(),
        Arc::clone(&handle_cache),
    )
    .with_session_coordinator(Arc::clone(&coordinator));

    let metadata = timeout(Duration::from_secs(10), protocol.probe(&magnet_url)).await??;
    assert_eq!(metadata.file_name, "payload.bin");
    assert_eq!(metadata.file_size, Some(payload.len() as u64));

    // 仅用 Session::get 不能证明 coordinator 是 acquisition owner；这里保留 exact Added
    // registration/provenance 断言。该 accessor 尚不存在时，编译错误即为本 RED 的预期信号。
    assert!(
        coordinator.has_registration_for_test(info_hash_bytes),
        "cache-miss production probe must register exact Added provenance in coordinator"
    );

    let binding_key = protocol.binding_key_for(&magnet_url);
    assert!(
        handle_cache.contains_key(&binding_key),
        "successful cache-miss probe must register a compatible cached handle"
    );

    // 后续 owner 必须从同一 protocol/coordinator 发起后台 cleanup；请求入口必须同步
    // 摘除当前 cache binding，避免后台删除完成前后续 probe 命中 stale handle。
    assert!(
        protocol.request_background_cleanup_for(&magnet_url),
        "successful probe must accept the coordinator-owned cleanup request"
    );
    assert!(
        !handle_cache.contains_key(&binding_key),
        "background cleanup request must remove the current cached handle binding"
    );

    // 从同一 protocol/coordinator 取得 action，观察已启动的 single-flight cleanup 结果，
    // 而不是再次发起删除。
    let cleanup = protocol
        .cleanup_action_for(&magnet_url)
        .expect("background cleanup request must retain its coordinator cleanup action");
    let cleanup_outcome = timeout(
        TEST_TIMEOUT,
        cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT),
    )
    .await??;
    assert_eq!(cleanup_outcome, BtCleanupOutcome::Converged);
    assert!(
        client_session.get(Hash(info_hash)).is_none(),
        "coordinator cleanup must remove the exact Added torrent from Session"
    );
    assert!(
        !coordinator.has_registration_for_test(info_hash_bytes),
        "coordinator must clear the exact registration after cleanup"
    );
    assert!(
        coordinator
            .begin_acquire(AcquisitionRequest::for_test(info_hash_bytes))
            .is_ok(),
        "a converged probe cleanup must reclaim the coordinator lane"
    );
    Ok(())
}

/// Slice 3A 下一原子微切片：cache-miss 下载路径必须与 probe 同权，经 coordinator 取得
/// exact Added registration，并保留可取回的 cleanup action。
///
/// 当前 `download_range_stream` 在 handle_cache miss 时仍直接调用 raw
/// `add_magnet_to_session`（且 `initial_peers` 固定为空），不经 `session_coordinator`。
/// 生产路径应提供与 `acquire_magnet_for_probe_with_deadline` 对称的 download acquisition
/// helper，并由 `download_range_stream` cache-miss 调用它。
///
/// 本测试引用预期 helper `acquire_magnet_for_download_with_deadline`：当前不存在时
/// compile RED 即为缺失生产接缝的信号。helper 成功后必须留下 exact registration，
/// 且后续 owner 可通过同一 protocol/coordinator 取回 tracked cleanup。
///
/// 本测试只锁定 download acquisition ownership 这一原子合同；不扩展到
/// `download_full`/`download_full_stream`，也不退役 `stop_and_remove_torrent`。
#[tokio::test]
async fn production_download_range_stream_registers_cache_miss_added_with_session_coordinator()
-> Result<(), Box<dyn std::error::Error>> {
    let seeder_root = TempDir::new()?;
    let client_root = TempDir::new()?;
    let payload = (0..96u8).collect::<Vec<_>>();
    let payload_path = seeder_root.path().join("payload.bin");
    std::fs::write(&payload_path, &payload)?;

    let torrent = create_torrent(
        &payload_path,
        CreateTorrentOptions {
            name: None,
            piece_length: Some(16 * 1024),
        },
    )
    .await?;
    let torrent_bytes = torrent.as_bytes()?;
    let info_hash = torrent.info_hash();
    let info_hash_bytes = info_hash.0;
    let magnet_url = format!("magnet:?xt=urn:btih:{}", hex::encode(info_hash_bytes));

    // Seeder 只使用临时目录和固定的独立 listener 范围；不启用 DHT、持久化或 UPnP。
    let seeder_session = Session::new_with_opts(
        seeder_root.path().to_path_buf(),
        SessionOptions {
            disable_dht: true,
            persistence: None,
            listen_port_range: Some(45_100..45_200),
            enable_upnp_port_forwarding: false,
            ..Default::default()
        },
    )
    .await?;
    let seeder_handle = seeder_session
        .add_torrent(
            AddTorrent::from_bytes(torrent_bytes.clone()),
            Some(AddTorrentOptions {
                paused: false,
                output_folder: Some(seeder_root.path().to_string_lossy().into_owned()),
                overwrite: true,
                disable_trackers: true,
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .ok_or_else(|| std::io::Error::other("seeder torrent must be Added"))?;
    timeout(Duration::from_secs(5), seeder_handle.wait_until_completed()).await??;
    let seeder_port = seeder_session
        .tcp_listen_port()
        .ok_or_else(|| std::io::Error::other("seeder must expose a TCP listen port"))?;

    // Client 不预置 torrent 或 cache；download acquisition 必须真实走 cache miss。
    let client_session = Session::new_with_opts(
        client_root.path().to_path_buf(),
        SessionOptions {
            disable_dht: true,
            persistence: None,
            enable_upnp_port_forwarding: false,
            ..Default::default()
        },
    )
    .await?;
    let config = MagnetConfig {
        metadata_timeout_secs: 5,
        enable_dht: false,
        enable_upnp: false,
        trackers: Vec::new(),
        peer_addrs: vec![format!("127.0.0.1:{seeder_port}")],
        // S-01 默认拒绝 loopback; loopback seeder fixture 必须显式 opt-in。
        allow_private_peers: true,
        ..MagnetConfig::default()
    };

    let coordinator = crate::magnet::new_librqbit_session_coordinator(Arc::clone(&client_session));
    let handle_cache = Arc::new(DashMap::new());
    let protocol = crate::magnet::MagnetProtocol::new(
        Arc::clone(&client_session),
        config.clone(),
        client_root.path().to_path_buf(),
        Arc::clone(&handle_cache),
    )
    .with_session_coordinator(Arc::clone(&coordinator));

    let initial_peers = {
        let mut addrs = crate::magnet::parse_pe_from_magnet(&magnet_url);
        addrs.extend(
            config
                .peer_addrs
                .iter()
                .filter_map(|s| s.parse::<std::net::SocketAddr>().ok()),
        );
        addrs
    };
    let metadata_deadline = Instant::now() + Duration::from_secs(config.metadata_timeout_secs);

    // 预期生产 helper：与 probe 对称，cache-miss download 必须经 coordinator 取得
    // exact Added。当前符号不存在 → compile RED。
    let crate::magnet::DownloadAcquisition { handle, cleanup } =
        crate::magnet::acquire_magnet_for_download_with_deadline(
            &coordinator,
            &magnet_url,
            client_root.path(),
            None,
            initial_peers,
            None,
            None,
            false,
            metadata_deadline,
        )
        .await?;

    // helper 返回后 drop cleanup 不得注销 lane；download 成功路径与 probe 一样保留
    // registration，供后续 owner 通过 cleanup_action_for 接管。
    drop(cleanup);
    drop(handle);

    assert!(
        coordinator.has_registration_for_test(info_hash_bytes),
        "cache-miss production download acquisition must register exact Added provenance in coordinator"
    );

    // 后续 owner 必须从同一 protocol/coordinator 发起后台 cleanup。
    assert!(
        protocol.request_background_cleanup_for(&magnet_url),
        "successful download acquisition must accept the coordinator-owned cleanup request"
    );

    let cleanup = protocol
        .cleanup_action_for(&magnet_url)
        .expect("background cleanup request must retain its coordinator cleanup action");
    let cleanup_outcome = timeout(
        TEST_TIMEOUT,
        cleanup.cleanup_until(Instant::now() + TEST_TIMEOUT),
    )
    .await??;
    assert_eq!(cleanup_outcome, BtCleanupOutcome::Converged);
    assert!(
        client_session.get(Hash(info_hash)).is_none(),
        "coordinator cleanup must remove the exact Added torrent from Session"
    );
    assert!(
        !coordinator.has_registration_for_test(info_hash_bytes),
        "coordinator must clear the exact registration after cleanup"
    );
    assert!(
        coordinator
            .begin_acquire(AcquisitionRequest::for_test(info_hash_bytes))
            .is_ok(),
        "a converged download cleanup must reclaim the coordinator lane"
    );
    Ok(())
}

/// 并发 cache-miss 回归:多个 download_range_stream worker 同时 miss 缓存时,
/// 不能因 coordinator exclusive lane fail-closed 而确定性失败。
///
/// singleflight 门闩串行化同一 bind_key 的 acquisition:首个 worker 完成
/// acquisition + cache insert 后,后续 worker 命中缓存复用 handle。
#[tokio::test]
async fn concurrent_download_range_stream_cache_miss_does_not_fail_closed() {
    use futures::future::join_all;
    use tachyon_core::traits::Protocol as _;

    let seeder_root = TempDir::new().unwrap();
    let client_root = TempDir::new().unwrap();
    let payload = (0..96u8).collect::<Vec<_>>();
    let payload_path = seeder_root.path().join("payload.bin");
    std::fs::write(&payload_path, &payload).unwrap();

    let torrent = create_torrent(
        &payload_path,
        CreateTorrentOptions {
            name: None,
            piece_length: Some(16 * 1024),
        },
    )
    .await
    .unwrap();
    let torrent_bytes = torrent.as_bytes().unwrap();
    let info_hash = torrent.info_hash();
    let info_hash_bytes = info_hash.0;
    let magnet_url = format!("magnet:?xt=urn:btih:{}", hex::encode(info_hash_bytes));

    let seeder_session = Session::new_with_opts(
        seeder_root.path().to_path_buf(),
        SessionOptions {
            disable_dht: true,
            persistence: None,
            listen_port_range: Some(45_200..45_300),
            enable_upnp_port_forwarding: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let seeder_handle = seeder_session
        .add_torrent(
            AddTorrent::from_bytes(torrent_bytes.clone()),
            Some(AddTorrentOptions {
                paused: false,
                output_folder: Some(seeder_root.path().to_string_lossy().into_owned()),
                overwrite: true,
                disable_trackers: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    timeout(Duration::from_secs(5), seeder_handle.wait_until_completed())
        .await
        .unwrap()
        .unwrap();
    let seeder_port = seeder_session.tcp_listen_port().unwrap();

    let client_session = Session::new_with_opts(
        client_root.path().to_path_buf(),
        SessionOptions {
            disable_dht: true,
            persistence: None,
            enable_upnp_port_forwarding: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let config = MagnetConfig {
        metadata_timeout_secs: 5,
        enable_dht: false,
        enable_upnp: false,
        trackers: Vec::new(),
        peer_addrs: vec![format!("127.0.0.1:{seeder_port}")],
        // S-01 默认拒绝 loopback; loopback seeder fixture 必须显式 opt-in。
        allow_private_peers: true,
        ..MagnetConfig::default()
    };
    let coordinator = crate::magnet::new_librqbit_session_coordinator(Arc::clone(&client_session));
    let handle_cache = Arc::new(DashMap::new());
    let protocol = Arc::new(
        crate::magnet::MagnetProtocol::new(
            Arc::clone(&client_session),
            config,
            client_root.path().to_path_buf(),
            Arc::clone(&handle_cache),
        )
        .with_session_coordinator(Arc::clone(&coordinator)),
    );

    // 并发发起 4 个 cache-miss download_range_stream:首个完成 acquisition 后其余应命中缓存。
    let mut futures = Vec::new();
    for i in 0..4u64 {
        let p = Arc::clone(&protocol);
        let url = magnet_url.clone();
        futures.push(async move {
            let stream = p.download_range_stream(&url, i * 16, i * 16 + 15, None).await;
            stream.is_ok()
        });
    }
    let results = join_all(futures).await;
    let success_count = results.iter().filter(|&&ok| ok).count();
    assert!(
        success_count >= 2,
        "并发 cache-miss 至少 2 个 worker 应成功,实际 {success_count}/4"
    );

    // 清理 coordinator lane。
    assert!(protocol.request_background_cleanup_for(&magnet_url));
    let cleanup = protocol
        .cleanup_action_for(&magnet_url)
        .expect("cleanup action must be retained");
    let outcome = timeout(
        Duration::from_secs(2),
        cleanup.cleanup_until(Instant::now() + Duration::from_secs(2)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(outcome, BtCleanupOutcome::Converged);
    assert!(!coordinator.has_registration_for_test(info_hash_bytes));
}
