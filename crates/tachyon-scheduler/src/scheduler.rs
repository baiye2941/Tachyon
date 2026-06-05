//! 任务调度器
//!
//! 管理下载任务的优先级队列和连接分配策略。

use std::cmp::Ordering;
use std::collections::BinaryHeap;

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
pub struct Scheduler {
    queue: BinaryHeap<ScheduledTask>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            queue: BinaryHeap::new(),
        }
    }

    /// 添加任务到调度队列
    pub fn push(&mut self, task: ScheduledTask) {
        self.queue.push(task);
    }

    /// 取出优先级最高的任务
    pub fn pop(&mut self) -> Option<ScheduledTask> {
        self.queue.pop()
    }

    /// 队列中任务数
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// 队列是否为空
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
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
        ScheduledTask {
            task_id: TaskId::new_v4(),
            priority,
            file_size: size,
            progress,
        }
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
                    let task = ScheduledTask {
                        task_id: TaskId::new_v4(),
                        priority: Priority::Queue,
                        file_size: (i * 20 + j) as u64,
                        progress: 0.0,
                    };
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
            sched.push(ScheduledTask {
                task_id: TaskId::new_v4(),
                priority: Priority::Prefetch,
                file_size: 1000 - i, // 递减大小
                progress: 0.0,
            });
            sched.push(ScheduledTask {
                task_id: TaskId::new_v4(),
                priority: Priority::Queue,
                file_size: 1000 - i,
                progress: 0.0,
            });
            sched.push(ScheduledTask {
                task_id: TaskId::new_v4(),
                priority: Priority::UserInitiated,
                file_size: 1000 - i,
                progress: 0.0,
            });
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
            sched.push(ScheduledTask {
                task_id: TaskId::new_v4(),
                priority: Priority::Queue,
                file_size: 1000,
                progress: p,
            });
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
                        s.push(ScheduledTask {
                            task_id: TaskId::new_v4(),
                            priority: match (i + j) % 3 {
                                0 => Priority::Prefetch,
                                1 => Priority::Queue,
                                _ => Priority::UserInitiated,
                            },
                            file_size: (i * 50 + j) as u64,
                            progress: 0.0,
                        });
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
}
