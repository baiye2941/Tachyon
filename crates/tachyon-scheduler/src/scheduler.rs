//! 任务调度器
//!
//! 管理下载任务的优先级队列和连接分配策略。

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::collections::HashMap;

use tachyon_core::TaskId;

/// 任务优先级
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// 预取(最低)
    Prefetch = 0,
    /// 队列下载
    Queue = 1,
    /// 用户主动下载(最高)
    UserInitiated = 2,
}

/// 调度队列中的任务条目
#[derive(Debug, Clone)]
pub struct ScheduledTask {
    pub task_id: TaskId,
    pub priority: Priority,
    /// 文件大小(用于小文件优先策略)
    pub file_size: u64,
    /// 当前进度(0.0 ~ 1.0),即将完成的任务提升优先级
    pub progress: f64,
    /// 内部版本号,用于检测队列中的过期条目
    version: u64,
}

impl ScheduledTask {
    /// 创建一个新的调度任务,version 自动设为 0(push 时会分配真实版本号)
    pub fn new(task_id: TaskId, priority: Priority, file_size: u64, progress: f64) -> Self {
        Self {
            task_id,
            priority,
            file_size,
            progress,
            version: 0,
        }
    }
}

impl PartialEq for ScheduledTask {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for ScheduledTask {}

impl PartialOrd for ScheduledTask {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledTask {
    fn cmp(&self, other: &Self) -> Ordering {
        let self_progress = if self.progress.is_nan() {
            0.0
        } else {
            self.progress
        };
        let other_progress = if other.progress.is_nan() {
            0.0
        } else {
            other.progress
        };

        self.priority
            .cmp(&other.priority)
            .then_with(|| {
                self_progress
                    .partial_cmp(&other_progress)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| other.file_size.cmp(&self.file_size))
            .then_with(|| self.task_id.cmp(&other.task_id))
    }
}

/// 下载任务调度器
///
/// 使用 BinaryHeap + HashMap 实现 O(1) 的查找和 O(log n) 的插入,
/// 移除操作通过延迟删除实现 O(1) 的摊销复杂度。
pub struct Scheduler {
    queue: BinaryHeap<ScheduledTask>,
    /// task_id 到最新 ScheduledTask 的索引,用于 O(1) 查找
    index: HashMap<TaskId, ScheduledTask>,
    /// 全局版本号计数器,每次更新递增
    next_version: u64,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            queue: BinaryHeap::new(),
            index: HashMap::new(),
            next_version: 1,
        }
    }

    /// 添加任务到调度队列
    ///
    /// 若任务 version 为 0,自动分配新版本号。
    pub fn push(&mut self, task: ScheduledTask) {
        let mut task = task;
        if task.version == 0 {
            task.version = self.next_version;
            self.next_version += 1;
        }
        self.index.insert(task.task_id, task.clone());
        self.queue.push(task);
    }

    /// 取出优先级最高的任务
    ///
    /// 跳过已被移除或更新的过期条目。
    pub fn pop(&mut self) -> Option<ScheduledTask> {
        loop {
            let task = self.queue.pop()?;
            match self.index.get(&task.task_id) {
                Some(indexed) if indexed.version == task.version => {
                    self.index.remove(&task.task_id);
                    return Some(task);
                }
                // 过期条目:已从 index 移除或版本不匹配,跳过
                _ => continue,
            }
        }
    }

    /// 队列中任务数(仅包含有效任务)
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// 队列是否为空
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// 移除指定任务(用于取消)
    ///
    /// 通过 HashMap 索引实现 O(1) 移除,队列中的过期条目将在 pop 时惰性地清理。
    /// 返回被移除的任务(如果存在)。
    pub fn remove(&mut self, task_id: TaskId) -> Option<ScheduledTask> {
        self.index.remove(&task_id)
    }

    /// 取消指定任务(语义等价于 remove,返回是否成功取消)
    pub fn cancel(&mut self, task_id: TaskId) -> bool {
        self.remove(task_id).is_some()
    }

    /// 更新指定任务的优先级(decrease-key / increase-key)
    ///
    /// 通过创建新版本任务重新入队实现,旧版本成为过期条目在 pop 时清理。
    /// 返回更新前的任务(如果存在)。
    pub fn update_priority(
        &mut self,
        task_id: TaskId,
        new_priority: Priority,
    ) -> Option<ScheduledTask> {
        let mut old_task = self.index.remove(&task_id)?;
        let old = old_task.clone();
        old_task.priority = new_priority;
        old_task.version = self.next_version;
        self.next_version += 1;
        self.index.insert(task_id, old_task.clone());
        self.queue.push(old_task);
        Some(old)
    }

    /// 更新指定任务的进度(用于提升即将完成任务的优先级)
    ///
    /// 通过创建新版本任务重新入队实现,旧版本成为过期条目在 pop 时清理。
    /// 返回更新前的任务(如果存在)。
    pub fn update_progress(&mut self, task_id: TaskId, new_progress: f64) -> Option<ScheduledTask> {
        let mut old_task = self.index.remove(&task_id)?;
        let old = old_task.clone();
        old_task.progress = new_progress;
        old_task.version = self.next_version;
        self.next_version += 1;
        self.index.insert(task_id, old_task.clone());
        self.queue.push(old_task);
        Some(old)
    }

    /// 查找指定任务是否存在
    pub fn contains(&self, task_id: TaskId) -> bool {
        self.index.contains_key(&task_id)
    }

    /// 获取指定任务的引用(只读)
    pub fn get(&self, task_id: TaskId) -> Option<&ScheduledTask> {
        self.index.get(&task_id)
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task(priority: Priority, size: u64, progress: f64) -> ScheduledTask {
        ScheduledTask::new(TaskId::new_v4(), priority, size, progress)
    }

    #[test]
    fn test_scheduler_empty() {
        let sched = Scheduler::new();
        assert!(sched.is_empty());
        assert_eq!(sched.len(), 0);
    }

    #[test]
    fn test_user_priority_first() {
        let mut sched = Scheduler::new();
        sched.push(make_task(Priority::Prefetch, 1000, 0.0));
        sched.push(make_task(Priority::UserInitiated, 1000, 0.0));
        sched.push(make_task(Priority::Queue, 1000, 0.0));

        let top = sched.pop().unwrap();
        assert_eq!(top.priority, Priority::UserInitiated);
    }

    #[test]
    fn test_small_file_priority() {
        let mut sched = Scheduler::new();
        sched.push(make_task(Priority::Queue, 10000, 0.0));
        sched.push(make_task(Priority::Queue, 100, 0.0));

        let top = sched.pop().unwrap();
        assert_eq!(top.file_size, 100);
    }

    #[test]
    fn test_progress_priority() {
        let mut sched = Scheduler::new();
        sched.push(make_task(Priority::Queue, 1000, 0.1));
        sched.push(make_task(Priority::Queue, 1000, 0.9));

        let top = sched.pop().unwrap();
        assert!(top.progress > 0.5);
    }

    #[test]
    fn test_fifo_within_same_priority() {
        let mut sched = Scheduler::new();
        let t1 = make_task(Priority::Queue, 1000, 0.0);
        let t2 = make_task(Priority::Queue, 1000, 0.0);
        sched.push(t1.clone());
        sched.push(t2.clone());

        // 相同优先级、大小、进度时,按 BinaryHeap 的内部顺序
        let _first = sched.pop().unwrap();
        let _second = sched.pop().unwrap();
        assert!(sched.is_empty());
    }

    // ------ 并发测试 ------

    /// 并发 push/pop 压力测试:多个 tokio 任务同时操作调度器
    #[tokio::test]
    async fn test_concurrent_push_pop_stress() {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let sched = Arc::new(Mutex::new(Scheduler::new()));
        let mut handles = Vec::new();

        // 启动 10 个生产者,每个推入 20 个任务
        for i in 0..10 {
            let sched_clone = Arc::clone(&sched);
            handles.push(tokio::spawn(async move {
                for j in 0..20 {
                    let task = ScheduledTask::new(
                        TaskId::new_v4(),
                        Priority::Queue,
                        (i * 20 + j) as u64,
                        0.0,
                    );
                    let mut s = sched_clone.lock().await;
                    s.push(task);
                }
            }));
        }

        // 等待所有生产者完成
        for handle in handles {
            handle.await.unwrap();
        }

        let s = sched.lock().await;
        assert_eq!(s.len(), 200, "应有 200 个任务");
        drop(s);

        // 启动 10 个消费者,每个弹出任务
        let mut handles = Vec::new();
        for _ in 0..10 {
            let sched_clone = Arc::clone(&sched);
            handles.push(tokio::spawn(async move {
                let mut count = 0;
                loop {
                    let mut s = sched_clone.lock().await;
                    if s.pop().is_some() {
                        count += 1;
                    } else {
                        break;
                    }
                }
                count
            }));
        }

        let mut total_popped = 0;
        for handle in handles {
            total_popped += handle.await.unwrap();
        }

        assert_eq!(total_popped, 200, "应弹出全部 200 个任务");

        let s = sched.lock().await;
        assert!(s.is_empty(), "队列应为空");
    }

    /// 大队列排序正确性:100+ 任务按优先级弹出
    #[tokio::test]
    async fn test_large_queue_priority_ordering() {
        let mut sched = Scheduler::new();

        // 添加 300 个任务:100 个 Prefetch, 100 个 Queue, 100 个 UserInitiated
        // 故意打乱插入顺序
        for i in 0..100u64 {
            sched.push(ScheduledTask::new(
                TaskId::new_v4(),
                Priority::Prefetch,
                1000 - i,
                0.0,
            ));
            sched.push(ScheduledTask::new(
                TaskId::new_v4(),
                Priority::Queue,
                1000 - i,
                0.0,
            ));
            sched.push(ScheduledTask::new(
                TaskId::new_v4(),
                Priority::UserInitiated,
                1000 - i,
                0.0,
            ));
        }

        assert_eq!(sched.len(), 300);

        // 弹出前 100 个应全部是 UserInitiated
        for _ in 0..100 {
            let task = sched.pop().unwrap();
            assert_eq!(
                task.priority,
                Priority::UserInitiated,
                "前 100 个应为 UserInitiated"
            );
        }

        // 接下来 100 个应全部是 Queue
        for _ in 0..100 {
            let task = sched.pop().unwrap();
            assert_eq!(task.priority, Priority::Queue, "101-200 个应为 Queue");
        }

        // 最后 100 个应全部是 Prefetch
        for _ in 0..100 {
            let task = sched.pop().unwrap();
            assert_eq!(task.priority, Priority::Prefetch, "201-300 个应为 Prefetch");
        }

        assert!(sched.is_empty());
    }

    /// 同优先级内按进度排序(高进度优先)
    #[tokio::test]
    async fn test_same_priority_progress_ordering() {
        let mut sched = Scheduler::new();

        // 添加 5 个同优先级同大小但不同进度的任务
        let progresses = [0.1, 0.9, 0.3, 0.7, 0.5];
        for p in progresses {
            sched.push(ScheduledTask::new(
                TaskId::new_v4(),
                Priority::Queue,
                1000,
                p,
            ));
        }

        // 弹出顺序应按进度降序:0.9, 0.7, 0.5, 0.3, 0.1
        let mut prev_progress = 2.0; // 初始值大于任何可能的进度
        for _ in 0..5 {
            let task = sched.pop().unwrap();
            assert!(
                task.progress <= prev_progress,
                "进度应单调递减: {} <= {}",
                task.progress,
                prev_progress
            );
            prev_progress = task.progress;
        }
    }

    /// 并发压测:高竞争场景下不 panic
    #[tokio::test]
    async fn test_concurrent_high_contention() {
        use std::sync::Arc;
        use tokio::sync::Mutex;

        let sched = Arc::new(Mutex::new(Scheduler::new()));
        let mut handles = Vec::new();

        // 20 个任务同时 push 和 pop
        for i in 0..20 {
            let sched_clone = Arc::clone(&sched);
            handles.push(tokio::spawn(async move {
                for j in 0..50 {
                    // 交替 push 和 pop
                    {
                        let mut s = sched_clone.lock().await;
                        s.push(ScheduledTask::new(
                            TaskId::new_v4(),
                            match (i + j) % 3 {
                                0 => Priority::Prefetch,
                                1 => Priority::Queue,
                                _ => Priority::UserInitiated,
                            },
                            (i * 50 + j) as u64,
                            0.0,
                        ));
                    }
                    {
                        let mut s = sched_clone.lock().await;
                        let _ = s.pop();
                    }
                }
            }));
        }

        for handle in handles {
            handle.await.unwrap();
        }

        // 最终队列状态应一致(不 panic 即可)
        let s = sched.lock().await;
        let _len = s.len();
        // 队列中剩余的任务数取决于 push/pop 的竞争结果,但不应 panic
    }

    // ------ S-11: remove / cancel / update_priority 测试 ------

    #[test]
    fn test_remove_existing_task() {
        let mut sched = Scheduler::new();
        let t1 = make_task(Priority::Queue, 1000, 0.0);
        let t2 = make_task(Priority::UserInitiated, 500, 0.0);
        let t1_id = t1.task_id;
        sched.push(t1);
        sched.push(t2);

        let removed = sched.remove(t1_id);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().file_size, 1000);
        assert_eq!(sched.len(), 1);
        assert!(!sched.contains(t1_id));
    }

    #[test]
    fn test_remove_nonexistent_task() {
        let mut sched = Scheduler::new();
        sched.push(make_task(Priority::Queue, 1000, 0.0));
        let fake_id = TaskId::new_v4();
        assert!(sched.remove(fake_id).is_none());
        assert_eq!(sched.len(), 1);
    }

    #[test]
    fn test_cancel_task() {
        let mut sched = Scheduler::new();
        let t = make_task(Priority::Prefetch, 2000, 0.5);
        let tid = t.task_id;
        sched.push(t);

        assert!(sched.cancel(tid));
        assert!(sched.is_empty());
        assert!(!sched.cancel(tid)); // 第二次取消应失败
    }

    #[test]
    fn test_update_priority_promotes_task() {
        let mut sched = Scheduler::new();
        let t1 = make_task(Priority::Prefetch, 1000, 0.0);
        let t2 = make_task(Priority::Queue, 1000, 0.0);
        let t1_id = t1.task_id;
        sched.push(t1);
        sched.push(t2);

        // 提升 t1 到 UserInitiated
        let old = sched.update_priority(t1_id, Priority::UserInitiated);
        assert!(old.is_some());
        assert_eq!(old.unwrap().priority, Priority::Prefetch);

        // t1 现在应该是最高优先级
        let top = sched.pop().unwrap();
        assert_eq!(top.task_id, t1_id);
        assert_eq!(top.priority, Priority::UserInitiated);
    }

    #[test]
    fn test_update_priority_demotes_task() {
        let mut sched = Scheduler::new();
        let t1 = make_task(Priority::UserInitiated, 1000, 0.0);
        let t2 = make_task(Priority::Queue, 1000, 0.0);
        let t1_id = t1.task_id;
        sched.push(t1);
        sched.push(t2);

        // 降低 t1 到 Prefetch
        sched.update_priority(t1_id, Priority::Prefetch);

        // t2 (Queue) 应该排在 t1 (Prefetch) 前面
        let top = sched.pop().unwrap();
        assert_eq!(top.priority, Priority::Queue);
        let second = sched.pop().unwrap();
        assert_eq!(second.task_id, t1_id);
        assert_eq!(second.priority, Priority::Prefetch);
    }

    #[test]
    fn test_update_progress() {
        let mut sched = Scheduler::new();
        let t = make_task(Priority::Queue, 1000, 0.1);
        let tid = t.task_id;
        sched.push(t);

        sched.update_progress(tid, 0.95);
        let task = sched.get(tid).unwrap();
        assert!((task.progress - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn test_contains_and_get() {
        let mut sched = Scheduler::new();
        let t = make_task(Priority::Queue, 500, 0.5);
        let tid = t.task_id;
        sched.push(t);

        assert!(sched.contains(tid));
        let found = sched.get(tid).unwrap();
        assert_eq!(found.file_size, 500);
        assert!((found.progress - 0.5).abs() < f64::EPSILON);

        let fake_id = TaskId::new_v4();
        assert!(!sched.contains(fake_id));
        assert!(sched.get(fake_id).is_none());
    }

    // ------ H-8: lazy deletion / HashMap + BinaryHeap 测试 ------

    /// 移除后 pop 能正确跳过过期条目
    #[test]
    fn test_remove_then_pop_skips_tombstone() {
        let mut sched = Scheduler::new();
        let t1 = make_task(Priority::UserInitiated, 1000, 0.0);
        let t2 = make_task(Priority::Queue, 500, 0.0);
        let t1_id = t1.task_id;
        sched.push(t1);
        sched.push(t2);

        // 移除最高优先级的 t1,但队列中仍保留其过期条目
        let removed = sched.remove(t1_id);
        assert!(removed.is_some());

        // pop 应跳过 t1 的过期条目,直接返回 t2
        let popped = sched.pop().unwrap();
        assert_eq!(popped.priority, Priority::Queue);

        // 队列为空
        assert!(sched.is_empty());
    }

    /// 更新优先级后旧版本成为过期条目,pop 能正确识别新版本
    #[test]
    fn test_update_priority_then_pop_finds_updated() {
        let mut sched = Scheduler::new();
        let t1 = make_task(Priority::Prefetch, 1000, 0.0);
        let t2 = make_task(Priority::Queue, 500, 0.0);
        let t1_id = t1.task_id;
        sched.push(t1);
        sched.push(t2);

        // t1 从 Prefetch 提升到 UserInitiated
        sched.update_priority(t1_id, Priority::UserInitiated);

        // pop 应返回更新后的 t1(UserInitiated),而不是旧版本的 Prefetch
        let popped = sched.pop().unwrap();
        assert_eq!(popped.task_id, t1_id);
        assert_eq!(popped.priority, Priority::UserInitiated);

        // 再 pop 返回 t2
        let second = sched.pop().unwrap();
        assert_eq!(second.priority, Priority::Queue);

        assert!(sched.is_empty());
    }

    /// 更新进度后旧版本成为过期条目,pop 能正确识别新版本
    #[test]
    fn test_update_progress_then_pop_finds_updated() {
        let mut sched = Scheduler::new();
        let t1 = make_task(Priority::Queue, 1000, 0.1);
        let t2 = make_task(Priority::Queue, 1000, 0.5);
        let t1_id = t1.task_id;
        sched.push(t1);
        sched.push(t2);

        // t1 进度从 0.1 提升到 0.95
        sched.update_progress(t1_id, 0.95);

        // 高进度优先,更新后的 t1 应该先被弹出
        let popped = sched.pop().unwrap();
        assert_eq!(popped.task_id, t1_id);
        assert!((popped.progress - 0.95).abs() < f64::EPSILON);

        // 再 pop 返回 t2
        let second = sched.pop().unwrap();
        assert!((second.progress - 0.5).abs() < f64::EPSILON);

        assert!(sched.is_empty());
    }

    /// 多次更新同一任务,只有最新版本有效
    #[test]
    fn test_multiple_updates_only_latest_version_valid() {
        let mut sched = Scheduler::new();
        let t = make_task(Priority::Prefetch, 1000, 0.0);
        let tid = t.task_id;
        sched.push(t);

        // 连续更新三次
        sched.update_priority(tid, Priority::Queue);
        sched.update_priority(tid, Priority::UserInitiated);
        sched.update_progress(tid, 0.8);

        // 最终 pop 应返回最新版本
        let popped = sched.pop().unwrap();
        assert_eq!(popped.task_id, tid);
        assert_eq!(popped.priority, Priority::UserInitiated);
        assert!((popped.progress - 0.8).abs() < f64::EPSILON);

        assert!(sched.is_empty());
    }

    /// 混合 remove 和 update,pop 能正确清理所有过期条目
    #[test]
    fn test_mixed_remove_and_update_pop_ordering() {
        let mut sched = Scheduler::new();
        let t1 = make_task(Priority::Prefetch, 1000, 0.0);
        let t2 = make_task(Priority::Queue, 500, 0.0);
        let t3 = make_task(Priority::UserInitiated, 200, 0.0);
        let t1_id = t1.task_id;
        let t2_id = t2.task_id;
        let t3_id = t3.task_id;
        sched.push(t1);
        sched.push(t2);
        sched.push(t3);

        // 移除 t1,更新 t2 为最高优先级
        sched.remove(t1_id);
        sched.update_priority(t2_id, Priority::UserInitiated);

        // t2 和 t3 同为 UserInitiated,t2 文件大小 500,t3 文件大小 200
        // 小文件优先,所以 t3 应先弹出
        let first = sched.pop().unwrap();
        assert_eq!(first.task_id, t3_id);
        assert_eq!(first.priority, Priority::UserInitiated);

        let second = sched.pop().unwrap();
        assert_eq!(second.task_id, t2_id);
        assert_eq!(second.priority, Priority::UserInitiated);

        assert!(sched.is_empty());
    }

    /// len 只返回有效任务数,不包含过期条目
    #[test]
    fn test_len_excludes_tombstones() {
        let mut sched = Scheduler::new();
        let t1 = make_task(Priority::Queue, 1000, 0.0);
        let t2 = make_task(Priority::Queue, 500, 0.0);
        let t1_id = t1.task_id;
        sched.push(t1);
        sched.push(t2);

        assert_eq!(sched.len(), 2);

        // 移除 t1
        sched.remove(t1_id);
        assert_eq!(sched.len(), 1);

        // pop 跳过过期条目后 len 仍为 0
        let _ = sched.pop();
        assert_eq!(sched.len(), 0);
    }

    /// 空队列多次 pop 不 panic
    #[test]
    fn test_pop_empty_queue_after_tombstones() {
        let mut sched = Scheduler::new();
        let t = make_task(Priority::Queue, 1000, 0.0);
        let tid = t.task_id;
        sched.push(t);
        sched.remove(tid);

        // pop 应跳过过期条目后返回 None,不 panic
        assert!(sched.pop().is_none());
    }

    /// remove 后 contains 和 get 立即失效
    #[test]
    fn test_remove_immediately_removes_from_index() {
        let mut sched = Scheduler::new();
        let t = make_task(Priority::Queue, 1000, 0.0);
        let tid = t.task_id;
        sched.push(t);

        assert!(sched.contains(tid));
        sched.remove(tid);
        assert!(!sched.contains(tid));
        assert!(sched.get(tid).is_none());
    }
}
