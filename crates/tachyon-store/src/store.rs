//! KV 存储抽象层
//!
//! 定义通用的 `Store` trait，为不同的存储后端提供统一接口。
//! 包含内存实现 (`MemoryStore`) 和文件实现 (`FileStore`)。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

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

/// 持久化写入模式
///
/// 控制写入后是否调用 fsync 保证数据落盘。
/// - `Fast`: 写入后不 fsync,性能最优但崩溃可能丢失最新数据
/// - `Durable`: 写入后 fsync 数据文件和目录,保证崩溃恢复
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Durability {
    /// 快速模式:不 fsync,依赖 OS 页面缓存回写
    #[default]
    Fast,
    /// 持久模式:数据文件 sync_all + 目录 sync,保证 crash-durable
    Durable,
}

/// 基于文件系统的 KV 存储（实现 `Store` trait）
///
/// 每个键对应一个 JSON 文件，存放在指定目录下。
/// 键经过安全转换后作为文件名（仅保留字母、数字和下划线）。
///
/// P2-10: 使用 OS 级文件锁（`.lock` 文件）防止跨实例/跨进程写冲突。
/// 第二个进程尝试打开同一目录时将失败并返回明确错误。
pub struct FileStore {
    dir: PathBuf,
    write_lock: RwLock<()>,
    durability: Durability,
    /// OS 级文件锁句柄，持有期间阻止其他进程打开同一 store 目录
    _lock_file: std::fs::File,
}

impl std::fmt::Debug for FileStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileStore")
            .field("dir", &self.dir)
            .field("durability", &self.durability)
            .finish_non_exhaustive()
    }
}

/// 全局临时文件计数器,确保每次写入使用唯一的临时文件名
static FILE_STORE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 临时文件清理守卫(RAII)
///
/// 持有临时文件路径,在 `Drop` 时删除该文件(忽略错误),保证 `write_entry`
/// 在任意失败路径(创建/写入/同步/重命名)下都不残留临时文件,避免磁盘垃圾累积。
///
/// 写入成功并完成重命名后应调用 [`TempGuard::disarm`] 取消清理:
/// 重命名已将临时文件移动至目标路径,此时无需再删除。
struct TempGuard {
    path: PathBuf,
    armed: bool,
}

impl TempGuard {
    /// 为指定路径创建守卫,初始处于"武装"状态(`Drop` 时删除文件)
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// 取消清理:写入成功且已重命名后调用,`Drop` 不再删除文件
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl FileStore {
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        Self::open_with_durability(dir, Durability::default())
    }

    /// 以指定持久化模式打开存储
    ///
    /// P2-10: 获取 OS 级文件锁，防止第二个进程打开同一 store 目录。
    /// 如果锁已被其他进程持有，返回 `ErrorKind::WouldBlock` 错误。
    pub fn open_with_durability(
        dir: impl AsRef<Path>,
        durability: Durability,
    ) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;

        // P2-10: 获取 OS 级排他锁
        // 锁文件仅用作 OS 级互斥句柄,不写入内容。
        // 使用 .append(true) 明确告知 clippy 我们不打算截断已存在的文件。
        let lock_path = dir.join(".lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock_file).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "存储目录已被其他进程占用: {} (锁文件: {})",
                    e,
                    lock_path.display()
                ),
            )
        })?;

        Ok(Self {
            dir,
            write_lock: RwLock::new(()),
            durability,
            _lock_file: lock_file,
        })
    }

    /// 获取存储目录
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 获取当前持久化模式
    ///
    /// 返回实例在 `open_with_durability` 时配置的模式。
    /// 注意: 即使返回 `Fast`,`set_durable` 仍可对单次写入强制 fsync。
    pub fn durability(&self) -> Durability {
        self.durability
    }

    /// 将键转换为安全的文件名
    ///
    /// 使用逐字节 percent-encoding: 保留 ASCII 字母数字、下划线和连字符,
    /// 其余字符先 UTF-8 编码再逐字节 `%XX` 转义。
    ///
    /// 这种编码方式确保:
    /// 1. 输出仅包含文件系统安全字符(ASCII 字母数字 + `_` + `-` + `%XX`)
    /// 2. 编码可逆(`unsafe_key` 可精确还原)
    /// 3. 多字节字符(如 CJK、emoji)按 UTF-8 字节逐字节编码,避免截断碰撞
    pub(crate) fn safe_key(key: &str) -> String {
        let mut result = String::with_capacity(key.len() * 2);
        for byte in key.bytes() {
            let ch = byte as char;
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                result.push(ch);
            } else {
                // 按字节 percent-encode,确保每字节恰好 2 位十六进制
                use std::fmt::Write;
                let _ = write!(result, "%{byte:02X}");
            }
        }
        result
    }

    /// 将安全文件名还原为原始键（safe_key 的逆操作）
    ///
    /// 将 `%XX` 序列解码为字节,其余字符按 UTF-8 字节处理,
    /// 最后将完整字节序列按 UTF-8 解码为字符串。
    pub(crate) fn unsafe_key(encoded: &str) -> String {
        let mut bytes: Vec<u8> = Vec::with_capacity(encoded.len());
        let mut chars = encoded.chars();
        while let Some(c) = chars.next() {
            if c == '%' {
                let hex: String = chars.by_ref().take(2).collect();
                if hex.len() == 2
                    && let Ok(byte) = u8::from_str_radix(&hex, 16)
                {
                    bytes.push(byte);
                    continue;
                }
                // 解码失败,保持原样
                bytes.push(b'%');
                for b in hex.bytes() {
                    bytes.push(b);
                }
            } else {
                // 安全字符均为 ASCII,直接推入字节
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                bytes.extend_from_slice(s.as_bytes());
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// 键对应的文件路径
    pub(crate) fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.json", Self::safe_key(key)))
    }

    /// 强制持久化写入(单次调用 fsync),不受实例 durability 配置影响
    ///
    /// 用于崩溃恢复场景:即使 `FileStore` 以 `Durability::Fast` 打开,
    /// 调用此方法仍会对本次写入执行 `sync_all`(数据文件 + 目录),
    /// 保证进程崩溃/断电后数据可恢复。
    ///
    /// 典型用途: `RecoveryManager` 的任务快照写入。
    pub fn set_durable(&self, key: &str, value: String) -> std::io::Result<()> {
        self.write_entry(key, &value, Durability::Durable)
    }

    /// 写入一个键值对的底层实现
    ///
    /// `effective` 为本次写入的实际持久化模式:
    /// - `Durable`: 走 File API + `sync_all` + 目录 sync
    /// - `Fast`: 走 `std::fs::write`,仅依赖 OS 页面缓存
    ///
    /// 抽取此方法使 `set`(跟随实例配置)与 `set_durable`(强制 Durable)
    /// 共享同一套原子写入逻辑,避免行为分叉。
    fn write_entry(&self, key: &str, value: &str, effective: Durability) -> std::io::Result<()> {
        let _guard = self.write_lock.write().map_err(|e| {
            tracing::warn!(key, error = %e, "KV 操作失败");
            std::io::Error::other(e.to_string())
        })?;
        std::fs::create_dir_all(&self.dir)?;
        let final_path = self.path_for(key);
        // 使用 pid + 计数器生成唯一临时文件名,避免多实例/多进程写冲突
        let temp_path = final_path.with_extension(format!(
            "tmp.{}-{}",
            std::process::id(),
            FILE_STORE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));

        // RAII 守卫:此后任一失败路径(创建/写入/同步/重命名)均自动清理 temp 文件,
        // 避免临时文件残留累积磁盘垃圾。成功重命名后通过 disarm 取消清理。
        let temp_guard = TempGuard::new(temp_path.clone());

        if effective == Durability::Durable {
            let mut file = std::fs::File::create(&temp_path).map_err(|e| {
                tracing::warn!(key, error = %e, "KV 创建临时文件失败");
                e
            })?;
            use std::io::Write;
            file.write_all(value.as_bytes()).map_err(|e| {
                tracing::warn!(key, error = %e, "KV 写入临时文件失败");
                e
            })?;
            file.sync_all().map_err(|e| {
                tracing::warn!(key, error = %e, "KV fsync 临时文件失败");
                e
            })?;
        } else {
            std::fs::write(&temp_path, value).map_err(|e| {
                tracing::warn!(key, error = %e, "KV 操作失败");
                e
            })?;
        }

        std::fs::rename(&temp_path, &final_path).map_err(|e| {
            tracing::warn!(key, error = %e, "KV rename 失败");
            e
        })?;

        // 重命名成功:临时文件已移动至目标路径,无需清理
        temp_guard.disarm();

        // 持久模式:重命名后同步目录以保证目录项更新落盘。
        // 审计 P-05:与 tachyon-io::dir_sync 对齐 —
        // Unix: open(dir)+sync_all 落盘目录项;
        // Windows: FILE_FLAG_BACKUP_SEMANTICS 打开验证可访问,NTFS 日志保证
        // rename 原子+目录项持久,无需(也不可靠)对目录 sync_all。
        // S-02b:失败必须传播(durable 承诺),不得仅 warn。
        if effective == Durability::Durable {
            sync_directory(&self.dir)?;
        }

        Ok(())
    }
}

/// 审计 P-05:目录项持久化,与 `tachyon-io::dir_sync::sync_parent_dir` 同构。
///
/// - Unix:打开目录并 `sync_all`
/// - Windows:以 `FILE_FLAG_BACKUP_SEMANTICS` 打开验证可访问后返回(不 fsync)
fn sync_directory(dir: &std::path::Path) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_BACKUP_SEMANTICS(0x0200_0000):允许打开目录句柄
        // share_mode READ|WRITE|DELETE(0x07):避免 sharing violation
        opts.custom_flags(0x0200_0000).share_mode(0x07);
    }
    let dir_file = opts.open(dir)?;
    #[cfg(not(target_os = "windows"))]
    {
        dir_file.sync_all()?;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = dir_file;
    }
    Ok(())
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
        self.write_entry(key, &value, self.durability)
    }

    fn delete(&self, key: &str) -> std::io::Result<bool> {
        let _guard = self.write_lock.write().map_err(|e| {
            tracing::warn!(key, error = %e, "KV 操作失败");
            std::io::Error::other(e.to_string())
        })?;
        let path = self.path_for(key);
        if path.exists() {
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
            if let Some(encoded_key) = name.strip_suffix(".json") {
                // 从编码后的文件名还原原始键
                let raw_key = Self::unsafe_key(encoded_key);
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

    /// 强制持久化存储可序列化值(单次调用 fsync)
    ///
    /// 即使 `KvStore` 以默认 `Durability::Fast` 打开,本方法也会对本次写入
    /// 执行 `sync_all`(数据文件 + 目录),保证崩溃后可恢复。
    ///
    /// 用于断点续传等对崩溃一致性有硬性要求的场景(见 `RecoveryManager`)。
    pub fn put_durable<V: Serialize>(&self, key: &str, value: &V) -> std::io::Result<()> {
        let json = serde_json::to_string(value)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.inner.set_durable(key, json)
    }

    /// 获取当前持久化模式
    pub fn durability(&self) -> Durability {
        self.inner.durability()
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
    ///
    /// 与 `Store::exists` 不同,此方法返回 `Result<bool>` 而非吞掉 I/O 错误,
    /// 允许调用方正确处理文件系统异常(权限拒绝、磁盘故障等)。
    pub fn contains(&self, key: &str) -> std::io::Result<bool> {
        self.inner.exists(key)
    }

    /// 列出匹配前缀的所有键
    pub fn list_by_prefix(&self, prefix: &str) -> std::io::Result<Vec<String>> {
        self.inner.keys(prefix)
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

    #[test]
    fn memory_poisoned_lock_returns_io_error_for_all_operations() {
        use std::sync::Arc;

        let store = Arc::new(MemoryStore::new());
        let poisoned = Arc::clone(&store);
        let result = std::thread::spawn(move || {
            let _guard = poisoned.data.write().unwrap();
            panic!("poison memory store lock");
        })
        .join();

        assert!(result.is_err());
        assert!(store.get("k").is_err());
        assert!(store.set("k", "v".to_string()).is_err());
        assert!(store.delete("k").is_err());
        assert!(store.exists("k").is_err());
        assert!(store.keys("").is_err());
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

    #[test]
    fn file_and_kv_dir_return_configured_root() {
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        let file_store = FileStore::open(tmp1.path()).unwrap();
        let kv_store = KvStore::open(tmp2.path()).unwrap();

        assert_eq!(file_store.dir(), tmp1.path());
        assert_eq!(kv_store.dir(), tmp2.path());
    }

    #[test]
    fn file_safe_key_escapes_path_sensitive_characters() {
        assert_eq!(FileStore::safe_key("task/a:b c"), "task%2Fa%3Ab%20c");
    }

    #[test]
    fn file_unsafe_key_roundtrip() {
        let original_keys = [
            "task/a:b c",
            "http://example.com/file.zip",
            "key_with-special_chars",
            "simple",
            "中文键名",
            "a/b/c/d:e",
        ];
        for key in original_keys {
            let encoded = FileStore::safe_key(key);
            let decoded = FileStore::unsafe_key(&encoded);
            assert_eq!(decoded, *key, "roundtrip failed for key: {key}");
        }
    }

    #[test]
    fn file_keys_returns_original_keys_with_special_chars() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        store.set("task/http://a.com/f", "1".to_string()).unwrap();
        store.set("task/simple", "2".to_string()).unwrap();

        let mut keys = store.keys("task/").unwrap();
        keys.sort();
        assert_eq!(keys, vec!["task/http://a.com/f", "task/simple"]);
    }

    #[test]
    fn file_keys_ignore_non_json_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        store.set("task_a", "1".to_string()).unwrap();
        std::fs::write(tmp.path().join("task_b.tmp"), "ignored").unwrap();
        std::fs::write(tmp.path().join("note.txt"), "ignored").unwrap();

        let keys = store.keys("task_").unwrap();
        assert_eq!(keys, vec!["task_a"]);
    }

    #[test]
    fn file_get_errors_when_key_path_is_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        std::fs::create_dir(store.path_for("as_dir")).unwrap();

        assert!(store.get("as_dir").is_err());
    }

    #[test]
    fn file_set_cleans_temp_when_rename_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        let final_path = store.path_for("blocked");
        let temp_path = final_path.with_extension("tmp");
        std::fs::create_dir(&final_path).unwrap();

        assert!(store.set("blocked", "value".to_string()).is_err());
        assert!(!temp_path.exists());
    }

    /// 收集目录中匹配临时文件命名模式的文件名(形如 `<key>.tmp.<pid>-<n>`)
    ///
    /// `write_entry` 生成的临时文件名为 `path_for(key)` 去掉 `.json` 后追加
    /// `tmp.<pid>-<n>`,故统一以子串 `.tmp.` 作为识别标志,排除 `.lock` 与 `.json`。
    fn collect_temp_files(dir: &Path) -> Vec<String> {
        let mut names = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.contains(".tmp.") {
                    names.push(name);
                }
            }
        }
        names
    }

    /// B7-temp: 正常写入(Fast 模式)后目录中不应残留任何临时文件
    ///
    /// 多次写入触发多次临时文件创建与 disarm 清理,随后扫描整个目录断言
    /// 无匹配 `.tmp.` 模式的残留文件,比逐键猜测 `.tmp` 路径更鲁棒。
    #[test]
    fn file_write_leaves_no_temp_pattern_in_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();

        for i in 0..5 {
            store.set(&format!("k{i}"), format!("v{i}")).unwrap();
        }

        let leftover = collect_temp_files(tmp.path());
        assert!(leftover.is_empty(), "目录中残留临时文件: {leftover:?}");
    }

    /// B7-temp: `set_durable` 正常写入后目录中不应残留任何临时文件
    #[test]
    fn file_set_durable_leaves_no_temp_pattern_in_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();

        for i in 0..5 {
            store
                .set_durable(&format!("dk{i}"), format!("v{i}"))
                .unwrap();
        }

        let leftover = collect_temp_files(tmp.path());
        assert!(leftover.is_empty(), "目录中残留临时文件: {leftover:?}");
    }

    /// B7-temp: rename 失败路径应清理真正的临时文件
    ///
    /// 将目标路径设为目录迫使 `rename` 失败,随后扫描目录断言无以 `blocked.tmp`
    /// 开头的残留文件,覆盖 RAII 守卫 `Drop` 的清理路径(此前仅 `rename` 失败
    /// 显式清理,`write_all`/`sync_all` 失败路径未清理,现由守卫统一兜底)。
    #[test]
    fn file_set_cleans_real_temp_when_rename_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        let final_path = store.path_for("blocked");
        // 将目标路径创建为目录,使 rename 无法用文件覆盖目录而失败
        std::fs::create_dir(&final_path).unwrap();

        assert!(store.set("blocked", "value".to_string()).is_err());

        let leftover: Vec<_> = collect_temp_files(tmp.path())
            .into_iter()
            .filter(|n| n.starts_with("blocked.tmp"))
            .collect();
        assert!(
            leftover.is_empty(),
            "rename 失败后残留临时文件: {leftover:?}"
        );
    }

    #[test]
    fn file_poisoned_write_lock_returns_io_error() {
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(FileStore::open(tmp.path()).unwrap());
        let poisoned = Arc::clone(&store);
        let result = std::thread::spawn(move || {
            let _guard = poisoned.write_lock.write().unwrap();
            panic!("poison file store lock");
        })
        .join();

        assert!(result.is_err());
        assert!(store.set("k", "v".to_string()).is_err());
        assert!(store.delete("k").is_err());
    }

    /// P2-10: 验证同一目录不允许第二个 FileStore 实例打开
    #[test]
    fn file_cross_instance_lock_prevents_second_open() {
        let tmp = tempfile::tempdir().unwrap();
        let _store1 = FileStore::open(tmp.path()).unwrap();

        // 第二个实例尝试打开同一目录应失败
        let result = FileStore::open(tmp.path());
        assert!(result.is_err(), "第二个 FileStore 不应成功打开同一目录");
        let err = result.unwrap_err();
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::WouldBlock,
            "错误类型应为 WouldBlock: {err}"
        );
    }

    /// P2-10: 验证第一个实例 drop 后，第二个实例可以打开
    #[test]
    fn file_cross_instance_lock_released_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let _store1 = FileStore::open(tmp.path()).unwrap();
            // _store1 在此处 drop，释放 OS 级锁
        }
        // 第一个实例已 drop，第二个应能成功打开
        let store2 = FileStore::open(tmp.path());
        assert!(store2.is_ok(), "第一个实例 drop 后，第二个应能成功打开");
    }

    #[test]
    fn file_atomic_write_survives_crash_simulation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();

        // 预写入旧值
        store
            .set("crash_key", r#"{"version":1}"#.to_string())
            .unwrap();

        // 模拟崩溃:写入 .tmp 文件(模拟半写),验证旧值仍可读
        let final_path = store.path_for("crash_key");
        let temp_path = final_path.with_extension("tmp");
        std::fs::write(&temp_path, "partial data").unwrap();
        // 不执行 rename,模拟进程崩溃

        // 旧值应仍然存在且完整
        let loaded = store.get("crash_key").unwrap();
        assert_eq!(loaded, Some(r#"{"version":1}"#.to_string()));

        // 清理残留 tmp
        let _ = std::fs::remove_file(&temp_path);
    }

    #[test]
    fn file_atomic_write_no_temp_leftover() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();

        store.set("new_key", r#"{"value":42}"#.to_string()).unwrap();

        let loaded = store.get("new_key").unwrap();
        assert_eq!(loaded, Some(r#"{"value":42}"#.to_string()));

        // 不应残留 .tmp 文件
        let tmp_path = store.path_for("new_key").with_extension("tmp");
        assert!(!tmp_path.exists(), "临时文件应已被 rename 清理");
    }

    #[test]
    fn file_atomic_write_overwrites_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();

        store
            .set("overwrite_key", r#"{"version":1}"#.to_string())
            .unwrap();
        store
            .set("overwrite_key", r#"{"version":2}"#.to_string())
            .unwrap();

        let loaded = store.get("overwrite_key").unwrap();
        assert_eq!(loaded, Some(r#"{"version":2}"#.to_string()));

        // 不应残留 .tmp 文件
        let tmp_path = store.path_for("overwrite_key").with_extension("tmp");
        assert!(!tmp_path.exists(), "覆盖后不应残留临时文件");
    }

    /// 审计 P-05:sync_directory 对存在的目录应成功(Windows 不 fsync,仅验证可打开)。
    #[test]
    fn file_sync_directory_succeeds_for_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        sync_directory(dir.path()).expect("存在的目录 sync 应成功");
    }

    /// 审计 P-05:不存在的目录应返回 Err。
    #[test]
    fn file_sync_directory_err_for_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-subdir");
        let err = sync_directory(&missing).expect_err("缺失目录应 Err");
        assert!(
            err.kind() == std::io::ErrorKind::NotFound
                || err.kind() == std::io::ErrorKind::PermissionDenied,
            "期望 NotFound/PermissionDenied, got {err:?}"
        );
    }

    // ── durability / set_durable 测试 ──

    #[test]
    fn file_open_default_is_fast() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        assert_eq!(store.durability(), Durability::Fast);
    }

    #[test]
    fn file_open_with_durability_durable() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open_with_durability(tmp.path(), Durability::Durable).unwrap();
        assert_eq!(store.durability(), Durability::Durable);
    }

    /// B7: `set_durable` 即使在 Fast 实例上也应写入成功且可读
    #[test]
    fn file_set_durable_writes_on_fast_instance() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();
        assert_eq!(store.durability(), Durability::Fast);

        store
            .set_durable("durable_key", r#"{"v":1}"#.to_string())
            .unwrap();

        let loaded = store.get("durable_key").unwrap();
        assert_eq!(loaded, Some(r#"{"v":1}"#.to_string()));
    }

    /// B7: `set_durable` 写入不应残留临时文件
    #[test]
    fn file_set_durable_no_temp_leftover() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();

        store
            .set_durable("durable_clean", r#"{"v":2}"#.to_string())
            .unwrap();

        let tmp_path = store.path_for("durable_clean").with_extension("tmp");
        assert!(!tmp_path.exists(), "set_durable 后不应残留临时文件");
    }

    /// B7: `set_durable` 应覆盖已有值
    #[test]
    fn file_set_durable_overwrites_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileStore::open(tmp.path()).unwrap();

        store.set("k", "v1".to_string()).unwrap();
        store.set_durable("k", "v2".to_string()).unwrap();

        assert_eq!(store.get("k").unwrap(), Some("v2".to_string()));
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
        assert!(!store.contains("x").unwrap());
        store.put("x", &"yes").unwrap();
        assert!(store.contains("x").unwrap());
    }

    #[test]
    fn kv_list_by_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        store.put("fav_bert-base-uncased", &"data1").unwrap();
        store.put("fav_gpt2", &"data2").unwrap();
        store.put("task_123", &"data3").unwrap();
        let favs = store.list_by_prefix("fav_").unwrap();
        assert_eq!(favs.len(), 2);
        assert!(favs.contains(&"fav_bert-base-uncased".to_string()));
        assert!(favs.contains(&"fav_gpt2".to_string()));
        assert!(!favs.contains(&"task_123".to_string()));
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

    #[test]
    fn kv_get_raw_returns_json_string() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        store.put("num", &42u32).unwrap();
        let raw = store.get_raw("num").unwrap();
        assert_eq!(raw, Some("42".to_string()));
    }

    #[test]
    fn kv_persistence_across_instances() {
        let tmp = tempfile::tempdir().unwrap();
        // 写入后关闭
        {
            let store = KvStore::open(tmp.path()).unwrap();
            store.put("persist", &"data".to_string()).unwrap();
        }
        // 重新打开,验证数据仍在
        let store = KvStore::open(tmp.path()).unwrap();
        let val: Option<String> = store.get("persist").unwrap();
        assert_eq!(val, Some("data".to_string()));
    }

    // ── KvStore put_durable 测试 ──

    /// B7: `KvStore::put_durable` 在 Fast 实例上写入并可读
    #[test]
    fn kv_put_durable_on_fast_instance_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        assert_eq!(store.durability(), Durability::Fast);

        #[derive(Serialize, Deserialize, Debug, PartialEq)]
        struct Cfg {
            v: u32,
        }
        store.put_durable("cfg", &Cfg { v: 7 }).unwrap();

        let loaded: Option<Cfg> = store.get("cfg").unwrap();
        assert_eq!(loaded, Some(Cfg { v: 7 }));
    }

    /// B7: `put_durable` 写入跨实例持久化(模拟崩溃后重开)
    #[test]
    fn kv_put_durable_persists_across_instances() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let store = KvStore::open(tmp.path()).unwrap();
            store.put_durable("persist", &"data".to_string()).unwrap();
        }
        // 重新打开,验证数据仍在(模拟进程崩溃后恢复)
        let store = KvStore::open(tmp.path()).unwrap();
        let val: Option<String> = store.get("persist").unwrap();
        assert_eq!(val, Some("data".to_string()));
    }

    /// B7: `put_durable` 覆盖已有值
    #[test]
    fn kv_put_durable_overwrites_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        store.put("k", &1u32).unwrap();
        store.put_durable("k", &2u32).unwrap();
        let val: Option<u32> = store.get("k").unwrap();
        assert_eq!(val, Some(2));
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
