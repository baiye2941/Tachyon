//! KV 存储抽象层
//!
//! 定义通用的 `Store` trait，为不同的存储后端提供统一接口。
//! 包含内存实现 (`MemoryStore`) 和文件实现 (`FileStore`)。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

// ── Store trait ──────────────────────────────────────────────────────

/// 通用 KV 存储接口
///
/// 所有值均以 `String` 形式存储（通常是 JSON 序列化后的字符串）。
/// 实现者需保证 `set` 和 `get` 的对称性：`set(k, v)` 后 `get(k)` 返回 `Some(v.clone())`。
pub trait Store {
    /// 获取指定键的值，不存在时返回 `None`
    fn get(&self, key: &str) -> std::io::Result<Option<String>>;

    /// 设置键值对，已存在时覆盖
    fn set(&self, key: &str, value: String) -> std::io::Result<()>;

    /// 删除键值对，返回是否确实删除了（键不存在时返回 `false`）
    fn delete(&self, key: &str) -> std::io::Result<bool>;

    /// 检查键是否存在
    fn exists(&self, key: &str) -> std::io::Result<bool>;

    /// 列出匹配前缀的所有键，空前缀返回全部键
    fn keys(&self, prefix: &str) -> std::io::Result<Vec<String>>;

    // ── 便捷方法 ──

    /// 存储可序列化类型（自动 JSON 序列化）
    fn put_typed<T: Serialize>(&self, key: &str, value: &T) -> std::io::Result<()> {
        let json = serde_json::to_string(value)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.set(key, json)
    }

    /// 读取可反序列化类型（自动 JSON 反序列化）
    fn get_typed<T: for<'de> Deserialize<'de>>(&self, key: &str) -> std::io::Result<Option<T>> {
        match self.get(key)? {
            None => Ok(None),
            Some(json) => {
                let value = serde_json::from_str(&json)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                Ok(Some(value))
            }
        }
    }
}

// ── MemoryStore ──────────────────────────────────────────────────────

/// 基于内存的 KV 存储
///
/// 数据保存在 `RwLock<HashMap>` 中，进程退出后丢失。
/// 适用于单元测试和不需要持久化的场景。
pub struct MemoryStore {
    data: RwLock<HashMap<String, String>>,
}

impl MemoryStore {
    /// 创建空的内存存储
    pub fn new() -> Self {
        Self {
            data: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Store for MemoryStore {
    fn get(&self, key: &str) -> std::io::Result<Option<String>> {
        let map = self.data.read().map_err(|e| {
            tracing::warn!(key, error = %e, "KV 操作失败");
            std::io::Error::other(e.to_string())
        })?;
        Ok(map.get(key).cloned())
    }

    fn set(&self, key: &str, value: String) -> std::io::Result<()> {
        let mut map = self.data.write().map_err(|e| {
            tracing::warn!(key, error = %e, "KV 操作失败");
            std::io::Error::other(e.to_string())
        })?;
        map.insert(key.to_string(), value);
        Ok(())
    }

    fn delete(&self, key: &str) -> std::io::Result<bool> {
        let mut map = self.data.write().map_err(|e| {
            tracing::warn!(key, error = %e, "KV 操作失败");
            std::io::Error::other(e.to_string())
        })?;
        Ok(map.remove(key).is_some())
    }

    fn exists(&self, key: &str) -> std::io::Result<bool> {
        let map = self.data.read().map_err(|e| {
            tracing::warn!(key, error = %e, "KV 操作失败");
            std::io::Error::other(e.to_string())
        })?;
        Ok(map.contains_key(key))
    }

    fn keys(&self, prefix: &str) -> std::io::Result<Vec<String>> {
        let map = self.data.read().map_err(|e| {
            tracing::warn!(prefix, error = %e, "KV 操作失败");
            std::io::Error::other(e.to_string())
        })?;
        Ok(map
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}

// ── FileStore ────────────────────────────────────────────────────────

/// 基于文件系统的 KV 存储（实现 `Store` trait）
///
/// 每个键对应一个 JSON 文件，存放在指定目录下。
/// 键经过安全转换后作为文件名（仅保留字母、数字和下划线）。
pub struct FileStore {
    dir: PathBuf,
}

impl FileStore {
    /// 创建或打开一个文件存储
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// 获取存储目录
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 将键转换为安全的文件名
    fn safe_key(key: &str) -> String {
        key.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    /// 键对应的文件路径
    fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.json", Self::safe_key(key)))
    }
}

impl Store for FileStore {
    fn get(&self, key: &str) -> std::io::Result<Option<String>> {
        let path = self.path_for(key);
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(&path).map_err(|e| {
            tracing::warn!(key, error = %e, "KV 操作失败");
            e
        })?;
        Ok(Some(data))
    }

    fn set(&self, key: &str, value: String) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.path_for(key), &value).map_err(|e| {
            tracing::warn!(key, error = %e, "KV 操作失败");
            e
        })
    }

    fn delete(&self, key: &str) -> std::io::Result<bool> {
        let path = self.path_for(key);
        if path.exists() {
            // Windows 上文件可能被其他进程/句柄占用,短暂重试
            #[cfg(target_os = "windows")]
            {
                let mut attempts = 0;
                loop {
                    match std::fs::remove_file(&path) {
                        Ok(()) => return Ok(true),
                        Err(e)
                            if e.kind() == std::io::ErrorKind::PermissionDenied && attempts < 5 =>
                        {
                            attempts += 1;
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                std::fs::remove_file(&path)?;
                Ok(true)
            }
        } else {
            Ok(false)
        }
    }

    fn exists(&self, key: &str) -> std::io::Result<bool> {
        Ok(self.path_for(key).exists())
    }

    fn keys(&self, prefix: &str) -> std::io::Result<Vec<String>> {
        let mut keys = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(key) = name.strip_suffix(".json") {
                // 从文件名还原键（safe_key 是单射的，这里只是去掉 .json 后缀）
                let raw_key = key.to_string();
                if raw_key.starts_with(prefix) {
                    keys.push(raw_key);
                }
            }
        }
        Ok(keys)
    }
}

// ── KvStore（旧实现，保持向后兼容）───────────────────────────────────

/// 嵌入式 KV 存储（旧接口，保持向后兼容）
///
/// 内部委托给 `FileStore`，提供泛型 `put`/`get` 方法。
pub struct KvStore {
    inner: FileStore,
}

impl KvStore {
    /// 创建或打开一个 KV 存储
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let inner = FileStore::open(dir)?;
        Ok(Self { inner })
    }

    /// 获取存储目录
    pub fn dir(&self) -> &Path {
        self.inner.dir()
    }

    /// 存储可序列化值
    pub fn put<V: Serialize>(&self, key: &str, value: &V) -> std::io::Result<()> {
        self.inner.put_typed(key, value)
    }

    /// 读取可反序列化值
    pub fn get<V: for<'de> Deserialize<'de>>(&self, key: &str) -> std::io::Result<Option<V>> {
        self.inner.get_typed(key)
    }

    /// 读取原始 JSON 字符串
    pub fn get_raw(&self, key: &str) -> std::io::Result<Option<String>> {
        self.inner.get(key)
    }

    /// 删除键
    pub fn delete(&self, key: &str) -> std::io::Result<bool> {
        self.inner.delete(key)
    }

    /// 列出所有键
    pub fn keys(&self) -> std::io::Result<Vec<String>> {
        self.inner.keys("")
    }

    /// 检查键是否存在
    pub fn contains(&self, key: &str) -> bool {
        self.inner.exists(key).unwrap_or(false)
    }
}

// ── 测试 ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── MemoryStore 测试 ──

    #[test]
    fn memory_set_and_get() {
        let store = MemoryStore::new();
        store.set("key", "value".to_string()).unwrap();
        assert_eq!(store.get("key").unwrap(), Some("value".to_string()));
    }

    #[test]
    fn memory_get_missing_key() {
        let store = MemoryStore::new();
        assert_eq!(store.get("nonexistent").unwrap(), None);
    }

    #[test]
    fn memory_delete_existing() {
        let store = MemoryStore::new();
        store.set("k", "v".to_string()).unwrap();
        assert!(store.delete("k").unwrap());
        assert_eq!(store.get("k").unwrap(), None);
    }

    #[test]
    fn memory_delete_nonexistent() {
        let store = MemoryStore::new();
        assert!(!store.delete("nope").unwrap());
    }

    #[test]
    fn memory_exists() {
        let store = MemoryStore::new();
        assert!(!store.exists("x").unwrap());
        store.set("x", "1".to_string()).unwrap();
        assert!(store.exists("x").unwrap());
    }

    #[test]
    fn memory_overwrite() {
        let store = MemoryStore::new();
        store.set("k", "v1".to_string()).unwrap();
        store.set("k", "v2".to_string()).unwrap();
        assert_eq!(store.get("k").unwrap(), Some("v2".to_string()));
    }

    #[test]
    fn memory_keys_prefix() {
        let store = MemoryStore::new();
        store.set("task_a", "1".to_string()).unwrap();
        store.set("task_b", "2".to_string()).unwrap();
        store.set("config_c", "3".to_string()).unwrap();

        let mut task_keys = store.keys("task_").unwrap();
        task_keys.sort();
        assert_eq!(task_keys, vec!["task_a", "task_b"]);

        let mut all_keys = store.keys("").unwrap();
        all_keys.sort();
        assert_eq!(all_keys, vec!["config_c", "task_a", "task_b"]);
    }

    #[test]
    fn memory_put_typed_and_get_typed() {
        let store = MemoryStore::new();
        #[derive(Serialize, Deserialize, Debug, PartialEq)]
        struct Cfg {
            name: String,
            val: u32,
        }
        let cfg = Cfg {
            name: "test".into(),
            val: 42,
        };
        store.put_typed("cfg", &cfg).unwrap();
        let loaded: Option<Cfg> = store.get_typed("cfg").unwrap();
        assert_eq!(loaded, Some(cfg));
    }

    #[test]
    fn memory_typed_missing_key() {
        let store = MemoryStore::new();
        let val: Option<String> = store.get_typed("nope").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn memory_empty_key() {
        let store = MemoryStore::new();
        store.set("", "empty_key".to_string()).unwrap();
        assert_eq!(store.get("").unwrap(), Some("empty_key".to_string()));
    }

    #[test]
    fn memory_empty_value() {
        let store = MemoryStore::new();
        store.set("k", String::new()).unwrap();
        assert_eq!(store.get("k").unwrap(), Some(String::new()));
    }

    // ── FileStore 测试 ──

    #[test]
    fn file_set_and_get() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        store.set("greeting", "hello".to_string()).unwrap();
        assert_eq!(store.get("greeting").unwrap(), Some("hello".to_string()));
    }

    #[test]
    fn file_get_missing_key() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        assert_eq!(store.get("nonexistent").unwrap(), None);
    }

    #[test]
    fn file_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        store.set("k", "v".to_string()).unwrap();
        assert!(store.delete("k").unwrap());
        assert!(!store.delete("k").unwrap());
    }

    #[test]
    fn file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        assert!(!store.exists("x").unwrap());
        store.set("x", "y".to_string()).unwrap();
        assert!(store.exists("x").unwrap());
    }

    #[test]
    fn file_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        store.set("k", "v1".to_string()).unwrap();
        store.set("k", "v2".to_string()).unwrap();
        assert_eq!(store.get("k").unwrap(), Some("v2".to_string()));
    }

    #[test]
    fn file_keys_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        store.set("task_a", "1".to_string()).unwrap();
        store.set("task_b", "2".to_string()).unwrap();
        store.set("cfg_c", "3".to_string()).unwrap();

        let mut task_keys = store.keys("task_").unwrap();
        task_keys.sort();
        assert_eq!(task_keys, vec!["task_a", "task_b"]);
    }

    #[test]
    fn file_put_typed_and_get_typed() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        #[derive(Serialize, Deserialize, Debug, PartialEq)]
        struct Config {
            name: String,
            value: u32,
        }
        let cfg = Config {
            name: "test".into(),
            value: 42,
        };
        store.put_typed("config", &cfg).unwrap();
        let loaded: Option<Config> = store.get_typed("config").unwrap();
        assert_eq!(loaded, Some(cfg));
    }

    #[test]
    fn file_empty_key() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        // 空键经 safe_key 转换后仍为空字符串，对应 _.json
        store.set("", "val".to_string()).unwrap();
        assert_eq!(store.get("").unwrap(), Some("val".to_string()));
    }

    #[test]
    fn file_empty_value() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        store.set("k", String::new()).unwrap();
        assert_eq!(store.get("k").unwrap(), Some(String::new()));
    }

    // ── KvStore 旧接口测试 ──

    #[test]
    fn kv_put_and_get() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        store.put("greeting", &"hello".to_string()).unwrap();
        let val: Option<String> = store.get("greeting").unwrap();
        assert_eq!(val, Some("hello".to_string()));
    }

    #[test]
    fn kv_get_missing_key() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let val: Option<String> = store.get("nonexistent").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn kv_delete() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        store.put("key", &42u32).unwrap();
        assert!(store.delete("key").unwrap());
        assert!(!store.delete("key").unwrap());
    }

    #[test]
    fn kv_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        store.put("a", &1).unwrap();
        store.put("b", &2).unwrap();
        store.put("c", &3).unwrap();
        let mut keys = store.keys().unwrap();
        keys.sort();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn kv_contains() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        assert!(!store.contains("x"));
        store.put("x", &"yes").unwrap();
        assert!(store.contains("x"));
    }

    #[test]
    fn kv_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        store.put("k", &"v1").unwrap();
        store.put("k", &"v2").unwrap();
        let val: Option<String> = store.get("k").unwrap();
        assert_eq!(val, Some("v2".to_string()));
    }

    #[test]
    fn kv_struct_value() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        #[derive(Serialize, Deserialize, Debug, PartialEq)]
        struct Config {
            name: String,
            value: u32,
        }
        let cfg = Config {
            name: "test".into(),
            value: 42,
        };
        store.put("config", &cfg).unwrap();
        let loaded: Option<Config> = store.get("config").unwrap();
        assert_eq!(loaded, Some(cfg));
    }

    // ── 并发测试 ──

    #[test]
    fn memory_concurrent_read_write() {
        use std::sync::Arc;
        use std::thread;

        let store = Arc::new(MemoryStore::new());
        let mut handles = Vec::new();

        // 写线程
        for i in 0..10 {
            let s = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                s.set(&format!("key_{i}"), format!("val_{i}")).unwrap();
            }));
        }

        // 读线程
        for i in 0..10 {
            let s = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                // 可能读到也可能读不到，但不应 panic
                let _ = s.get(&format!("key_{i}"));
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // 验证所有写入都生效
        for i in 0..10 {
            assert_eq!(
                store.get(&format!("key_{i}")).unwrap(),
                Some(format!("val_{i}"))
            );
        }
    }

    #[test]
    fn memory_concurrent_delete() {
        use std::sync::Arc;
        use std::thread;

        let store = Arc::new(MemoryStore::new());
        store.set("shared", "data".to_string()).unwrap();

        let mut handles = Vec::new();
        for _ in 0..5 {
            let s = Arc::clone(&store);
            handles.push(thread::spawn(move || {
                let _ = s.delete("shared");
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // 最终状态：键已被删除
        assert!(!store.exists("shared").unwrap());
    }
}
