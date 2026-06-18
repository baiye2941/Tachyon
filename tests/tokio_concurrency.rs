//! Tokio 并发原语集成测试
//!
//! 验证 JoinSet、Semaphore、Mutex、RwLock、mpsc、broadcast、watch、select!、
//! cancellation、timeout、panic 恢复、高并发压力、任务泄漏、死锁与饥饿等场景。
//! 所有涉时测试使用 `start_paused = true` 做确定性时间控制。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use tokio::sync::{Barrier, Mutex, RwLock, Semaphore, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;

// ===================== 1. JoinSet 测试 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn join_set_tasks_complete_and_aggregate_errors() {
    let mut set = JoinSet::new();
    set.spawn(async { Ok::<_, &'static str>(1) });
    set.spawn(async { Err::<i32, _>("boom") });
    set.spawn(async { Ok(3) });

    let mut sum_ok = 0i32;
    let mut errors = Vec::new();
    while let Some(res) = set.join_next().await {
        match res.expect("JoinSet task should not panic") {
            Ok(v) => sum_ok += v,
            Err(e) => errors.push(e),
        }
    }

    assert_eq!(sum_ok, 4, "成功结果应聚合为 1 + 3");
    assert_eq!(errors, vec!["boom"], "错误结果应被收集");
}

// ===================== 2. Semaphore 测试 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn semaphore_blocks_until_permit_released() {
    let sem = Arc::new(Semaphore::new(1));
    let permit = sem.acquire().await.expect("acquire should succeed");

    let sem2 = Arc::clone(&sem);
    let handle = tokio::spawn(async move {
        let start = Instant::now();
        let p = sem2
            .acquire()
            .await
            .expect("waiter should eventually acquire");
        drop(p);
        start.elapsed()
    });

    // 让等待者在信号量上注册，再推进时间并释放许可
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(50)).await;
    drop(permit);

    let elapsed = handle.await.expect("spawned task should complete");
    assert!(
        elapsed >= Duration::from_millis(50),
        "等待者应至少阻塞 50ms"
    );
    assert_eq!(sem.available_permits(), 1, "释放后许可应恢复");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn semaphore_fairness_first_waiter_wins() {
    let sem = Arc::new(Semaphore::new(1));
    let _permit = sem.acquire().await.unwrap();

    let order = Arc::new(Mutex::new(Vec::new()));

    let sem2 = Arc::clone(&sem);
    let order1 = Arc::clone(&order);
    let first = tokio::spawn(async move {
        let _p = sem2.acquire().await.unwrap();
        order1.lock().await.push(1);
    });
    tokio::task::yield_now().await;

    let sem3 = Arc::clone(&sem);
    let order2 = Arc::clone(&order);
    let second = tokio::spawn(async move {
        let _p = sem3.acquire().await.unwrap();
        order2.lock().await.push(2);
    });
    tokio::task::yield_now().await;

    drop(_permit);
    first.await.unwrap();
    second.await.unwrap();

    let o = order.lock().await;
    assert_eq!(o.len(), 2, "两个等待者都应完成");
    assert_eq!(o[0], 1, "公平信号量下先进队的等待者优先获得许可");
}

// ===================== 3. Mutex 竞争测试 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn mutex_contention_increments_counter_correctly() {
    let counter = Arc::new(Mutex::new(0));
    let mut handles = Vec::with_capacity(10);

    for _ in 0..10 {
        let c = Arc::clone(&counter);
        handles.push(tokio::spawn(async move {
            for _ in 0..100 {
                let mut guard = c.lock().await;
                *guard += 1;
            }
        }));
    }

    for h in handles {
        h.await.expect("task should not fail");
    }

    assert_eq!(*counter.lock().await, 1000, "10 个任务各加 100 次应为 1000");
}

// ===================== 4. RwLock 竞争测试 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn rwlock_many_readers_one_writer_no_data_race() {
    let data = Arc::new(RwLock::new(0));
    let mut readers = Vec::with_capacity(20);

    for _ in 0..20 {
        let d = Arc::clone(&data);
        readers.push(tokio::spawn(async move {
            let v = *d.read().await;
            assert!(v == 0 || v == 42, "读取的值只能是初始值或写后的值");
        }));
    }

    let data_for_writer = Arc::clone(&data);
    let writer = tokio::spawn(async move {
        let mut guard = data_for_writer.write().await;
        *guard = 42;
    });

    for h in readers {
        h.await.unwrap();
    }
    writer.await.unwrap();

    assert_eq!(*data.read().await, 42);
}

// ===================== 5. mpsc 测试 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn mpsc_multi_producer_single_consumer_closes_cleanly() {
    let (tx, mut rx) = mpsc::channel::<i32>(16);
    let mut handles = Vec::with_capacity(5);

    for i in 0..5 {
        let tx = tx.clone();
        handles.push(tokio::spawn(async move {
            for j in 0..10 {
                tx.send(i * 10 + j).await.expect("send should succeed");
            }
        }));
    }
    drop(tx);

    let mut received = Vec::new();
    while let Some(v) = rx.recv().await {
        received.push(v);
    }

    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(received.len(), 50, "5 个生产者各发 10 条共 50 条");
    let sum: i32 = received.iter().sum();
    let expected: i32 = (0..5).flat_map(|i| (0..10).map(move |j| i * 10 + j)).sum();
    assert_eq!(sum, expected, "所有消息值的总和应一致");
}

// ===================== 6. broadcast 测试 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn broadcast_multi_receiver_and_lag_handling() {
    let (tx, mut rx1) = broadcast::channel::<i32>(4);
    let mut rx2 = tx.subscribe();

    tx.send(1).unwrap();
    tx.send(2).unwrap();
    tx.send(3).unwrap();

    assert_eq!(rx1.recv().await.unwrap(), 1);
    assert_eq!(rx1.recv().await.unwrap(), 2);
    assert_eq!(rx2.recv().await.unwrap(), 1);
    assert_eq!(rx2.recv().await.unwrap(), 2);

    // 填满并溢出缓冲区，使 rx1 出现 lag
    for i in 4..=10 {
        tx.send(i).unwrap();
    }

    let res = rx1.recv().await;
    assert!(
        matches!(res, Err(broadcast::error::RecvError::Lagged(_))),
        "消费落后的接收者应收到 Lagged 错误: {res:?}"
    );

    // lag 之后仍能接收最新消息（缓冲区保留最近 capacity 条，此处从 7 开始）
    assert_eq!(rx1.recv().await.unwrap(), 7);
    assert_eq!(rx1.recv().await.unwrap(), 8);
    assert_eq!(rx1.recv().await.unwrap(), 9);
    assert_eq!(rx1.recv().await.unwrap(), 10);
}

// ===================== 7. watch 测试 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn watch_skips_old_values_and_notifies_changes() {
    let (tx, mut rx) = watch::channel(0);
    tx.send(1).unwrap();
    tx.send(2).unwrap();

    // borrow_and_update 直接返回最新值并跳过中间旧值
    assert_eq!(*rx.borrow_and_update(), 2);

    tx.send(3).unwrap();
    rx.changed().await.expect("changed should succeed");
    assert_eq!(*rx.borrow(), 3);
}

// ===================== 8. select! 测试 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn select_races_channels_and_prefers_cancel_branch() {
    let (tx1, mut rx1) = oneshot::channel::<i32>();
    let (_tx2, mut rx2) = oneshot::channel::<i32>();
    tx1.send(1).unwrap();

    let result = tokio::select! {
        v = &mut rx1 => v.expect("rx1 should complete"),
        v = &mut rx2 => v.expect("rx2 should complete"),
    };
    assert_eq!(result, 1);

    // 第二个 select 验证 biased 模式下优先分支先被取消/完成
    let (tx3, rx3) = oneshot::channel::<i32>();
    let (_tx4, rx4) = oneshot::channel::<i32>();
    tx3.send(10).unwrap();

    let result = tokio::select! {
        biased;
        v = rx3 => v.unwrap(),
        _ = rx4 => 99,
    };
    assert_eq!(result, 10, "biased 模式下应先检查已完成的第一分支");
}

// ===================== 9. cancellation 测试 =====================

struct DropCounter(Arc<AtomicUsize>);

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cancellation_cleans_up_resources_and_aborts_task() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let (tx, mut rx) = mpsc::channel::<()>(1);

    let d = Arc::clone(&dropped);
    let handle = tokio::spawn(async move {
        let _guard = DropCounter(d);
        // 让主任务有机会确认资源已创建
        tokio::task::yield_now().await;
        let _ = rx.recv().await;
        42
    });

    // 确保子任务已创建 DropCounter 后再中止
    tokio::task::yield_now().await;

    // 使用 AbortHandle 中止任务
    let abort = handle.abort_handle();
    abort.abort();
    let err = handle.await.expect_err("aborted task should return Err");
    assert!(err.is_cancelled(), "错误应为取消类型");
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "任务取消后内部资源应被 drop"
    );

    // 接收端已随任务取消而 drop，发送端应检测到通道关闭
    assert!(tx.send(()).await.is_err(), "发送端应检测到通道关闭");
}

// ===================== 10. timeout 测试 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn timeout_triggers_correctly() {
    // 超时场景：用 join! 同时驱动 timeout 与手动推进时间
    let (timed_out, _) = tokio::join!(
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::time::sleep(Duration::from_secs(5))
        ),
        async { tokio::time::advance(Duration::from_secs(2)).await }
    );
    assert!(timed_out.is_err(), "1s 超时应对 5s sleep 触发");

    // 不超时场景
    let (finished, _) = tokio::join!(
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::time::sleep(Duration::from_millis(500))
        ),
        async { tokio::time::advance(Duration::from_millis(600)).await }
    );
    assert!(finished.is_ok(), "500ms sleep 应在 1s 超时前完成");
}

// ===================== 11. task panic 恢复测试 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn join_set_panic_does_not_stop_other_tasks() {
    let mut set = JoinSet::new();
    set.spawn(async { 1 });
    set.spawn(async { panic!("intentional panic") });
    set.spawn(async { 3 });

    let mut panicked = false;
    let mut sum = 0i32;
    while let Some(res) = set.join_next().await {
        match res {
            Ok(v) => sum += v,
            Err(e) => {
                assert!(e.is_panic(), "应捕获 panic 类型的 JoinError");
                panicked = true;
            }
        }
    }

    assert!(panicked, "应有一个任务 panic");
    assert_eq!(sum, 4, "其余任务结果应继续累加");
}

// ===================== 12. 高并发压力测试 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn high_concurrency_stress_no_panic_or_deadlock() {
    const N: i32 = 150;
    let counter = Arc::new(Mutex::new(0));
    let mut set = JoinSet::new();

    for i in 0..N {
        let c = Arc::clone(&counter);
        set.spawn(async move {
            let mut guard = c.lock().await;
            *guard += 1;
            i
        });
    }

    let mut sum = 0i32;
    while let Some(res) = set.join_next().await {
        sum += res.expect("高并发下任务不应 panic");
    }

    assert_eq!(sum, (0..N).sum::<i32>(), "所有任务返回值总和应一致");
    assert_eq!(*counter.lock().await, N, "共享计数器应被正确递增 {N} 次");
}

// ===================== 13. 任务泄漏检测 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn task_leak_detection_via_weak_reference() {
    let strong = Arc::new(AtomicUsize::new(0));
    let weak: Weak<AtomicUsize> = Arc::downgrade(&strong);

    let handle = tokio::spawn(async move {
        let _inner = strong;
        42
    });

    assert_eq!(handle.await.unwrap(), 42);
    assert!(
        weak.upgrade().is_none(),
        "任务结束后其内部 Arc 应被释放，弱引用无法升级"
    );
}

// ===================== 14. 死锁检测 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn two_phase_lock_ordering_avoids_deadlock() {
    let a = Arc::new(Mutex::new(0));
    let b = Arc::new(Mutex::new(0));
    let mut handles = Vec::with_capacity(10);

    for _ in 0..10 {
        let a = Arc::clone(&a);
        let b = Arc::clone(&b);
        handles.push(tokio::spawn(async move {
            // 所有任务按相同顺序获取锁，避免循环等待
            let mut ga = a.lock().await;
            *ga += 1;
            let mut gb = b.lock().await;
            *gb += 1;
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    assert_eq!(*a.lock().await, 10);
    assert_eq!(*b.lock().await, 10);
}

// ===================== 15. 饥饿检测 =====================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn writer_progress_among_many_readers() {
    let lock = Arc::new(RwLock::new(0));
    let barrier = Arc::new(Barrier::new(21));
    let mut readers = Vec::with_capacity(20);

    for _ in 0..20 {
        let l = Arc::clone(&lock);
        let b = Arc::clone(&barrier);
        readers.push(tokio::spawn(async move {
            let guard = l.read().await;
            b.wait().await;
            drop(guard);
        }));
    }

    let lock_for_writer = Arc::clone(&lock);
    let writer = tokio::spawn(async move {
        barrier.wait().await;
        let mut guard = lock_for_writer.write().await;
        *guard = 42;
    });

    for h in readers {
        h.await.unwrap();
    }
    writer.await.unwrap();

    assert_eq!(*lock.read().await, 42, "写锁最终应完成更新");
}
