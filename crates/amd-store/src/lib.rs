//! AI Model Downloader 持久化存储层
//!
//! 嵌入式 KV 存储，用于持久化:
//! - 下载任务状态
//! - 分片进度
//! - 配置
//! - DHT 节点
//!
//! 实现基于文件系统的简单 KV 存储，无需外部数据库依赖。
//! 每个 key 对应一个 JSON 文件，存放在指定目录下。
//!
//! ## 模块结构
//!
//! - [`store`]: `Store` trait 抽象、`MemoryStore`（内存）、`FileStore`（文件）
//! - [`kv`]: `KvStore` 旧接口（向后兼容，内部委托给 `FileStore`）
//! - [`recovery`][]: 断点续传恢复管理（`TaskSnapshot`、`RecoveryManager`）

pub mod kv;
pub mod recovery;
pub mod store;

pub use kv::KvStore;
pub use recovery::{RecoveryManager, TaskRecord, TaskSnapshot};
pub use store::{FileStore, MemoryStore, Store};

// 验证测试:放在 crate 根级别，以便 `--exact` 匹配

/// 验证断点续传恢复:保存任务 -> 模拟崩溃 -> 恢复未完成任务
#[cfg(test)]
#[test]
fn recovery() {
    use recovery::{RecoveryManager, TaskRecord};

    let tmp = tempfile::tempdir().unwrap();

    // 阶段 1:保存 3 个任务（2 个未完成，1 个已完成）
    {
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        mgr.save_task(&TaskRecord {
            task_id: "t1".into(),
            url: "https://example.com/a.zip".into(),
            save_path: "/downloads/a.zip".into(),
            file_size: Some(1024),
            downloaded: 512,
            completed_fragments: vec![0, 1],
            total_fragments: 4,
            status: "downloading".into(),
        })
        .unwrap();
        mgr.save_task(&TaskRecord {
            task_id: "t2".into(),
            url: "https://example.com/b.zip".into(),
            save_path: "/downloads/b.zip".into(),
            file_size: Some(2048),
            downloaded: 2048,
            completed_fragments: vec![0, 1, 2, 3],
            total_fragments: 4,
            status: "completed".into(),
        })
        .unwrap();
        mgr.save_task(&TaskRecord {
            task_id: "t3".into(),
            url: "https://example.com/c.zip".into(),
            save_path: "/downloads/c.zip".into(),
            file_size: Some(4096),
            downloaded: 1024,
            completed_fragments: vec![0],
            total_fragments: 8,
            status: "paused".into(),
        })
        .unwrap();
    }

    // 阶段 2:模拟重启，从存储中恢复
    {
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let pending = mgr.recover_pending_tasks().unwrap();
        assert_eq!(pending.len(), 2, "应恢复 2 个未完成任务");

        let ids: Vec<&str> = pending.iter().map(|r| r.task_id.as_str()).collect();
        assert!(ids.contains(&"t1"), "downloading 状态应恢复");
        assert!(ids.contains(&"t3"), "paused 状态应恢复");

        // 验证恢复的数据完整性
        let t1 = mgr.load_task("t1").unwrap().unwrap();
        assert_eq!(t1.downloaded, 512);
        assert_eq!(t1.completed_fragments, vec![0, 1]);

        // 验证已完成任务不在恢复列表中
        let completed = mgr.load_task("t2").unwrap().unwrap();
        assert_eq!(completed.status, "completed");
    }
}
