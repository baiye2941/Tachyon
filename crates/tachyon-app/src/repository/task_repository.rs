use std::sync::Arc;

use dashmap::iter::Iter;
use dashmap::{DashMap, mapref::one::Ref, mapref::one::RefMut};

use crate::commands::TaskInfo;
use tachyon_core::types::DownloadState;

/// 任务仓库，封装 [`DashMap<String, TaskInfo>`] 的并发访问。
#[derive(Debug, Clone)]
pub struct TaskRepository {
    inner: Arc<DashMap<String, TaskInfo>>,
}

impl TaskRepository {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(DashMap::with_capacity(capacity)),
        }
    }

    pub fn get(&self, id: &str) -> Option<Ref<'_, String, TaskInfo>> {
        self.inner.get(id)
    }

    pub fn get_mut(&self, id: &str) -> Option<RefMut<'_, String, TaskInfo>> {
        self.inner.get_mut(id)
    }

    pub fn insert(&self, id: String, task: TaskInfo) -> Option<TaskInfo> {
        self.inner.insert(id, task)
    }

    pub fn remove(&self, id: &str) -> Option<(String, TaskInfo)> {
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
        }
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
