use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::iter::Iter;
use dashmap::{DashMap, mapref::one::Ref, mapref::one::RefMut};

use crate::commands::TaskInfo;
use tachyon_core::types::DownloadState;

/// 任务仓库，封装 [`DashMap<String, TaskInfo>`] 的并发访问。
///
/// 内置版本计数器(`version`)，每次 insert/remove/update_status 时递增。
/// ProgressBroker 可通过 `version()` 检测是否有变更，无变更时跳过全量扫描。
#[derive(Debug, Clone)]
pub struct TaskRepository {
    inner: Arc<DashMap<String, TaskInfo>>,
    version: Arc<AtomicU64>,
}

impl TaskRepository {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            version: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(DashMap::with_capacity(capacity)),
            version: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn get(&self, id: &str) -> Option<Ref<'_, String, TaskInfo>> {
        self.inner.get(id)
    }

    pub fn get_mut(&self, id: &str) -> Option<RefMut<'_, String, TaskInfo>> {
        self.inner.get_mut(id)
    }

    pub fn insert(&self, id: String, task: TaskInfo) -> Option<TaskInfo> {
        self.version.fetch_add(1, Ordering::Relaxed);
        self.inner.insert(id, task)
    }

    pub fn remove(&self, id: &str) -> Option<(String, TaskInfo)> {
        self.version.fetch_add(1, Ordering::Relaxed);
        self.inner.remove(id)
    }

    pub fn contains_key(&self, id: &str) -> bool {
        self.inner.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn iter(&self) -> Iter<'_, String, TaskInfo> {
        self.inner.iter()
    }

    pub fn update_status(&self, id: &str, state: DownloadState) {
        if let Some(mut task) = self.inner.get_mut(id) {
            task.status = state;
            self.version.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 返回当前版本号,每次 insert/remove/update_status 时递增
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Relaxed)
    }

    pub fn inner(&self) -> &Arc<DashMap<String, TaskInfo>> {
        &self.inner
    }
}

impl Default for TaskRepository {
    fn default() -> Self {
        Self::new()
    }
}
