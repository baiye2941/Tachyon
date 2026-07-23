//! 断点续传恢复管理
//!
//! 负责在应用启动时从持久化存储中恢复未完成的下载任务。
//! 提供 `TaskRecord` / `TaskSnapshot` 类型和 `RecoveryManager` 管理器。

use std::{
    collections::HashMap,
    fmt, io,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, IgnoredAny, MapAccess, Visitor},
};

use crate::kv::KvStore;

/// 当前快照 schema 版本号
///
/// 每次 TaskSnapshot 结构发生新增/删除/重命名字段时递增。
/// 新增字段必须标注 `#[serde(default)]`，确保旧版本 JSON 可正常反序列化。
/// 删除字段应先改为 `Option<T>` + `#[serde(default)]`，至少保留一个版本周期的兼容。
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 7;

fn default_supports_range_true() -> bool {
    true
}

/// 下载任务快照（用于断点续传）
///
/// 记录任务的完整状态，可在应用重启后恢复。
///
/// schema_version 字段用于版本检测和未来迁移:
/// - 旧 JSON(无 schemaVersion 字段)通过 `#[serde(default)]` 自动补为 0
/// - 新 JSON 带有 schemaVersion=1
/// - 迁移策略:若 schema_version < SNAPSHOT_SCHEMA_VERSION,可在加载后补填新字段默认值
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    /// schema 版本号,用于向前兼容检测
    ///
    /// 旧 JSON 不含此字段时默认为 0,加载后可检测并补填。
    #[serde(default)]
    pub schema_version: u32,
    /// 审计 H-05:单调 revision。full-save / patch 成功后 +1;旧 revision 不得覆盖新值。
    #[serde(default)]
    pub revision: u64,
    pub id: String,
    pub url: String,
    pub save_path: String,
    pub file_name: String,
    pub file_size: Option<u64>,
    #[serde(default)]
    pub downloaded: u64,
    #[serde(default)]
    pub completed_fragments: Vec<u32>,
    /// 未完整下载的分片及其已下载字节数(字节级断点续传)
    #[serde(default)]
    pub partial_fragments: HashMap<u32, u64>,
    #[serde(default)]
    pub total_fragments: u32,
    #[serde(default)]
    pub fragment_size: u64,
    pub status: tachyon_core::DownloadState,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    #[serde(default)]
    pub content_length: Option<u64>,
    /// 服务端是否支持 Range(HTTP 200 fallback 降级后为 false)。
    /// 缺字段默认 true:旧快照按可分片续传,与历史行为一致。
    #[serde(default = "default_supports_range_true")]
    pub supports_range: bool,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub fail_reason: Option<String>,
    #[serde(default)]
    pub retry_count: u32,
    /// 用户自定义任务标签(如 "important"、"model" 等),用于前端分组/过滤。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hf_meta: Option<serde_json::Value>,
    /// 任务在列表中的显示顺序,越小越靠前。
    /// 旧版快照无此字段时默认 0,保持与创建时间降序的兼容排序。
    #[serde(default)]
    pub display_order: i64,
    /// 创建任务时配置的镜像 URL 列表。
    /// 旧版快照无此字段时默认 None;重启/续传时从快照恢复多源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirror_urls: Option<Vec<String>>,
}

/// 下载任务持久化记录（旧接口，保持向后兼容）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    /// 任务 ID
    pub task_id: String,
    /// 下载 URL
    pub url: String,
    /// 保存路径
    pub save_path: String,
    /// 文件总大小（字节）
    pub file_size: Option<u64>,
    /// 已下载字节数
    pub downloaded: u64,
    /// 已完成的分片索引列表
    pub completed_fragments: Vec<u32>,
    /// 分片总数
    pub total_fragments: u32,
    /// 任务状态
    pub status: String,
}

impl From<TaskSnapshot> for TaskRecord {
    fn from(s: TaskSnapshot) -> Self {
        Self {
            task_id: s.id,
            url: s.url,
            save_path: s.save_path,
            file_size: s.file_size,
            downloaded: s.downloaded,
            completed_fragments: s.completed_fragments,
            total_fragments: s.total_fragments,
            status: format!("{:?}", s.status).to_lowercase(),
        }
    }
}

impl From<TaskRecord> for TaskSnapshot {
    fn from(r: TaskRecord) -> Self {
        Self {
            schema_version: 0, // 旧记录无 schema 版本,标记为 0 表示需要迁移
            revision: 0,
            id: r.task_id,
            url: r.url,
            save_path: r.save_path.clone(),
            file_name: std::path::Path::new(&r.save_path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown")
                .to_string(),
            file_size: r.file_size,
            downloaded: r.downloaded,
            completed_fragments: r.completed_fragments,
            partial_fragments: HashMap::new(),
            total_fragments: r.total_fragments,
            fragment_size: 0,
            status: parse_legacy_status(&r.status),
            etag: None,
            last_modified: None,
            content_length: r.file_size,
            supports_range: true,
            created_at: String::new(),
            updated_at: String::new(),
            fail_reason: None,
            retry_count: 0,
            tags: Vec::new(),
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        }
    }
}

fn parse_legacy_status(status: &str) -> tachyon_core::DownloadState {
    // A-02: 利用 strum::EnumString 自动派生的 FromStr，
    // 未知状态字符串回退到 Failed（兼容旧数据）。
    use std::str::FromStr;
    tachyon_core::DownloadState::from_str(status).unwrap_or(tachyon_core::DownloadState::Failed)
}

/// 被保护的 future schema 快照。
///
/// 该快照对当前版本有效，但需要更高版本的程序处理，不能被当作损坏数据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedSnapshot {
    /// 存储中的原始 key。
    pub key: String,
    /// 快照声明的 schema 版本。
    pub found_version: u32,
    /// 当前程序支持的最高 schema 版本。
    pub supported_version: u32,
}

/// 快照恢复的 fail-closed 错误。
pub enum RecoveryError {
    /// 快照由更高版本程序写入，必须保持原始内容不变。
    Unsupported(ProtectedSnapshot),
    /// 快照内容无效，包含其存储 key 以便调用方定位。
    InvalidData { key: String },
    /// 存储层 I/O 操作失败。
    Io(io::Error),
    /// S-02b:任务命名空间已被活跃 reservation 占用,普通 API 不得执行。
    ReservationActive,
}

impl fmt::Debug for RecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(snapshot) => formatter
                .debug_tuple("RecoveryError::Unsupported")
                .field(snapshot)
                .finish(),
            Self::InvalidData { key } => formatter
                .debug_struct("RecoveryError::InvalidData")
                .field("key", key)
                .finish(),
            Self::Io(_) => formatter.write_str("RecoveryError::Io(..)"),
            Self::ReservationActive => formatter.write_str("RecoveryError::ReservationActive"),
        }
    }
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(_) => formatter.write_str("不支持的快照 schema 版本"),
            Self::InvalidData { .. } => formatter.write_str("快照数据无效"),
            Self::Io(_) => formatter.write_str("快照存储 I/O 操作失败"),
            Self::ReservationActive => formatter.write_str("任务命名空间已被占用"),
        }
    }
}

impl std::error::Error for RecoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Unsupported(_) | Self::InvalidData { .. } | Self::ReservationActive => None,
        }
    }
}

/// 恢复结果:包含成功恢复的任务、损坏 key 和受保护的 future schema 快照。
///
/// 单个损坏 JSON 或 future schema 不会阻断其他任务的恢复(隔离策略)。
#[derive(Debug)]
pub struct RecoveryResult {
    /// 成功恢复的任务快照
    pub tasks: Vec<TaskSnapshot>,
    /// 无法解析的 key 列表(记录日志供排查,不中断恢复流程)
    pub corrupt_keys: Vec<String>,
    /// 需要更新程序才能处理的快照，原始数据保持不变。
    pub unsupported_schema: Vec<ProtectedSnapshot>,
}

/// 顶层 JSON 的 schema header 分类。
#[derive(Debug, Clone, Copy)]
enum SnapshotSchemaHeader {
    /// 缺少 header，保留旧格式 fallback。
    Legacy,
    /// 已声明的 schema 版本。
    Version(u32),
}

/// 仅接受无符号 32 位整数的 schemaVersion 值。
struct SchemaVersion(u32);

impl<'de> Deserialize<'de> for SchemaVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(SchemaVersionVisitor)
    }
}

struct SchemaVersionVisitor;

impl<'de> Visitor<'de> for SchemaVersionVisitor {
    type Value = SchemaVersion;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("0 到 u32::MAX 的整数 schemaVersion")
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        u32::try_from(value)
            .map(SchemaVersion)
            .map_err(|_| E::invalid_value(de::Unexpected::Unsigned(value), &self))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::invalid_value(de::Unexpected::Signed(value), &self))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::invalid_value(de::Unexpected::Float(value), &self))
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::invalid_value(de::Unexpected::Bool(value), &self))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::invalid_value(de::Unexpected::Str(value), &self))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::invalid_type(de::Unexpected::Unit, &self))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::invalid_type(de::Unexpected::Option, &self))
    }
}

struct SnapshotSchemaHeaderVisitor;

impl<'de> Visitor<'de> for SnapshotSchemaHeaderVisitor {
    type Value = SnapshotSchemaHeader;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("包含可选 schemaVersion 的 JSON 对象")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut schema_version = None;
        while let Some(field) = map.next_key::<String>()? {
            if field == "schemaVersion" {
                if schema_version.is_some() {
                    return Err(de::Error::duplicate_field("schemaVersion"));
                }
                schema_version = Some(map.next_value::<SchemaVersion>()?.0);
            } else {
                // 流式跳过非 header 字段，避免 Value/Map 折叠重复字段。
                map.next_value::<IgnoredAny>()?;
            }
        }

        Ok(schema_version.map_or(SnapshotSchemaHeader::Legacy, SnapshotSchemaHeader::Version))
    }
}

impl<'de> Deserialize<'de> for SnapshotSchemaHeader {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(SnapshotSchemaHeaderVisitor)
    }
}

/// 使用流式 visitor 分类顶层 schema header，并拒绝尾随 JSON。
fn classify_snapshot_schema(json: &str) -> io::Result<SnapshotSchemaHeader> {
    let mut deserializer = serde_json::Deserializer::from_str(json);
    let header = SnapshotSchemaHeader::deserialize(&mut deserializer)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    deserializer
        .end()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(header)
}

/// 单 key 加载的内部分类结果，供 batch 路径保留 future/corrupt/I/O 语义。
#[derive(Debug)]
enum LoadTaskSnapshotError {
    Io(io::Error),
    Unsupported { found_version: u32 },
    Corrupt,
}

impl From<io::Error> for LoadTaskSnapshotError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl LoadTaskSnapshotError {
    fn into_recovery_error(self, key: &str) -> RecoveryError {
        match self {
            Self::Io(error) => RecoveryError::Io(error),
            Self::Unsupported { found_version } => {
                RecoveryError::Unsupported(protected_snapshot(key, found_version))
            }
            Self::Corrupt => RecoveryError::InvalidData {
                key: key.to_string(),
            },
        }
    }
}

fn protected_snapshot(key: &str, found_version: u32) -> ProtectedSnapshot {
    ProtectedSnapshot {
        key: key.to_string(),
        found_version,
        supported_version: SNAPSHOT_SCHEMA_VERSION,
    }
}

fn ensure_supported_snapshot(snapshot: &TaskSnapshot) -> Result<(), RecoveryError> {
    if snapshot.schema_version > SNAPSHOT_SCHEMA_VERSION {
        return Err(RecoveryError::Unsupported(protected_snapshot(
            &format!("task_{}", snapshot.id),
            snapshot.schema_version,
        )));
    }
    Ok(())
}

/// S-02b:任务命名空间 reservation capability。
///
/// 由 [`RecoveryManager::reserve_task_namespace`] 创建,持有期间所有普通
/// load/save/update/remove/restore/batch API 返回
/// [`RecoveryError::ReservationActive`],仅 reserved 变体可用。
/// `Drop` 释放匹配的活跃 reservation;构造仅限 `RecoveryManager` 内部。
pub struct TaskNamespaceReservation<'a> {
    /// 创建该 reservation 的 manager 引用,Drop 时用于释放活跃 reservation。
    manager: &'a RecoveryManager,
    /// 该 reservation 的唯一 nonce,用于区分同一 manager 的多次 reservation。
    nonce: u64,
}

impl fmt::Debug for TaskNamespaceReservation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskNamespaceReservation")
            .field("nonce", &self.nonce)
            .finish_non_exhaustive()
    }
}

/// 恢复管理器
///
/// 负责任务快照的持久化与恢复。所有 `task_*` 键的写入均走强制 Durable 路径
/// (fsync 数据文件 + 目录),以满足崩溃恢复承诺(见 [`Self::save_task_snapshot`])。
pub struct RecoveryManager {
    store: KvStore,
    /// 序列化所有快照 mutation(full-save / patch / delete),防止并发覆盖
    progress_lock: std::sync::Mutex<()>,
    /// 审计 H-05:删除 tombstone。key=task_id, value=删除时磁盘 revision。
    /// 之后任何 `revision <= tombstone` 的 save 拒绝,防止旧 full-save 复活已删任务。
    delete_tombstones: std::sync::Mutex<HashMap<String, u64>>,
    /// S-02b:活跃 reservation 的 nonce;None 表示无活跃 reservation。
    active_reservation: std::sync::Mutex<Option<u64>>,
}

/// S-02b:全局递增的 reservation nonce,保证每次 reservation 唯一。
static NEXT_NONCE: AtomicU64 = AtomicU64::new(1);

impl RecoveryManager {
    /// 创建恢复管理器
    pub fn new(store: KvStore) -> Self {
        Self {
            store,
            progress_lock: std::sync::Mutex::new(()),
            delete_tombstones: std::sync::Mutex::new(HashMap::new()),
            active_reservation: std::sync::Mutex::new(None),
        }
    }

    /// S-02b:检查是否存在活跃 reservation;存在则返回 `ReservationActive`。
    ///
    /// 必须在持有 `progress_lock` 后调用,确保与 `reserve`/`Drop` 互斥。
    fn check_no_active_reservation(&self) -> Result<(), RecoveryError> {
        if self
            .active_reservation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return Err(RecoveryError::ReservationActive);
        }
        Ok(())
    }

    /// S-02b §3.1:扫描全部 `task_*` key 并用 header classifier 分类。
    ///
    /// 遇 future/invalid 即返回 typed error,不得创建 reservation。
    /// 全部合法时创建活跃 reservation 并返回。
    pub fn reserve_task_namespace(&self) -> Result<TaskNamespaceReservation<'_>, RecoveryError> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        // 先扫描全部 task_ key,遇第一个 bad key 即 fail-closed
        for key in self.store.keys().map_err(RecoveryError::Io)? {
            if key.starts_with("task_") {
                // 复用 load_task_snapshot_by_key 的分类逻辑(future/invalid/corrupt)
                self.load_task_snapshot_by_key(&key)
                    .map_err(|error| error.into_recovery_error(&key))?;
            }
        }
        // 全部合法,创建活跃 reservation
        let nonce = NEXT_NONCE.fetch_add(1, Ordering::Relaxed);
        *self
            .active_reservation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(nonce);
        Ok(TaskNamespaceReservation {
            manager: self,
            nonce,
        })
    }

    /// S-02b §3.1:验证 reservation 的 manager identity + nonce + active。
    fn validate_reservation(
        &self,
        reservation: &TaskNamespaceReservation<'_>,
    ) -> Result<(), RecoveryError> {
        // manager identity:通过指针比较验证 reservation 属于本 manager
        if !std::ptr::eq(reservation.manager, self) {
            return Err(RecoveryError::ReservationActive);
        }
        match self
            .active_reservation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            Some(active) if *active == reservation.nonce => Ok(()),
            _ => Err(RecoveryError::ReservationActive),
        }
    }

    /// S-02b:reserved load — 验证 reservation 后委托内部 locked 操作。
    pub fn load_reserved(
        &self,
        reservation: &TaskNamespaceReservation<'_>,
        task_id: &str,
    ) -> Result<Option<TaskSnapshot>, RecoveryError> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.validate_reservation(reservation)?;
        let key = format!("task_{task_id}");
        self.load_task_snapshot_by_key(&key)
            .map_err(|error| error.into_recovery_error(&key))
    }

    /// S-02b:reserved save — 验证 reservation 后强制写入(bump revision,跳过 CAS)。
    ///
    /// reservation 已提供独占性,无需 CAS revision check;tombstone 仍生效。
    pub fn save_reserved(
        &self,
        reservation: &TaskNamespaceReservation<'_>,
        snapshot: &TaskSnapshot,
    ) -> Result<(), RecoveryError> {
        ensure_supported_snapshot(snapshot)?;
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.validate_reservation(reservation)?;
        let key = format!("task_{}", snapshot.id);
        let existing = self
            .load_task_snapshot_by_key(&key)
            .map_err(|error| error.into_recovery_error(&key))?;
        // tombstone:拒绝基于删除前状态的旧写
        if let Ok(tombs) = self.delete_tombstones.lock()
            && let Some(&tomb_rev) = tombs.get(&snapshot.id)
            && snapshot.revision <= tomb_rev
        {
            tracing::warn!(
                task_id = %snapshot.id,
                incoming_revision = snapshot.revision,
                tombstone_revision = tomb_rev,
                "拒绝写入已删除任务快照(H-05 tombstone)"
            );
            return Ok(());
        }
        let base_rev = existing.as_ref().map(|s| s.revision).unwrap_or(0);
        let mut to_write = snapshot.clone();
        to_write.revision = base_rev.saturating_add(1);
        if to_write.schema_version < SNAPSHOT_SCHEMA_VERSION {
            to_write.schema_version = SNAPSHOT_SCHEMA_VERSION;
        }
        self.store
            .put_durable(&key, &to_write)
            .map_err(RecoveryError::Io)
    }

    /// S-02b:reserved update — 验证 reservation 后执行 locked load-modify-save。
    pub fn update_reserved(
        &self,
        reservation: &TaskNamespaceReservation<'_>,
        task_id: &str,
        patch: impl FnOnce(&mut TaskSnapshot),
    ) -> Result<Option<TaskSnapshot>, RecoveryError> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.validate_reservation(reservation)?;
        let key = format!("task_{task_id}");
        let mut snapshot = match self
            .load_task_snapshot_by_key(&key)
            .map_err(|error| error.into_recovery_error(&key))?
        {
            Some(s) => s,
            None => return Ok(None),
        };
        patch(&mut snapshot);
        if snapshot.schema_version < SNAPSHOT_SCHEMA_VERSION {
            snapshot.schema_version = SNAPSHOT_SCHEMA_VERSION;
        }
        self.save_task_snapshot_locked(&snapshot)?;
        let final_snap = self
            .load_task_snapshot_by_key(&key)
            .map_err(|error| error.into_recovery_error(&key))?;
        Ok(final_snap)
    }

    /// S-02b:reserved remove — 验证 reservation 后执行 locked remove。
    pub fn remove_reserved(
        &self,
        reservation: &TaskNamespaceReservation<'_>,
        task_id: &str,
    ) -> Result<bool, RecoveryError> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.validate_reservation(reservation)?;
        let key = format!("task_{task_id}");
        let existing_rev = self
            .load_task_snapshot_by_key(&key)
            .map_err(|error| error.into_recovery_error(&key))?
            .map(|s| s.revision)
            .unwrap_or(0);
        let deleted = self.store.delete(&key).map_err(RecoveryError::Io)?;
        if let Ok(mut tombs) = self.delete_tombstones.lock() {
            tombs.insert(task_id.to_string(), existing_rev);
        }
        Ok(deleted)
    }

    /// S-02b:reserved restore — 验证 reservation 后执行 locked restore。
    pub fn restore_reserved(
        &self,
        reservation: &TaskNamespaceReservation<'_>,
        snapshot: &TaskSnapshot,
    ) -> Result<(), RecoveryError> {
        ensure_supported_snapshot(snapshot)?;
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.validate_reservation(reservation)?;
        self.restore_task_snapshot_locked(snapshot)
    }

    /// 保存任务快照(强制持久化)
    ///
    /// 即使底层 `KvStore` 以 `Durability::Fast` 打开,本方法仍通过
    /// [`KvStore::put_durable`] 对本次写入执行 `sync_all`(数据文件 + 目录),
    /// 保证进程崩溃/断电后任务进度不丢失。
    ///
    /// # 为什么 RecoveryManager 必须 Durable
    ///
    /// `RecoveryManager` 的核心职责是崩溃恢复:应用重启后从这里重建未完成下载。
    /// 若快照写走 Fast 模式(仅依赖 OS 页面缓存),进程崩溃或断电时最新进度会丢失,
    /// 恢复时只能读到上一次落盘的旧进度,导致已下载分片被重复下载。
    ///
    /// # 为什么 Durable 在热路径可接受
    ///
    /// 生产热路径(`chunk_reader_pool.rs`)走 [`Self::update_snapshot`],
    /// 该方法已通过 `CHECKPOINT_BATCH_SIZE` 与 `PARTIAL_CHECKPOINT_INTERVAL`
    /// 限频(批量 + 时间间隔双维度节流),Durable 的 fsync 开销被摊薄到可控频率,
    /// 不会成为每分片的热点。
    pub fn save_task_snapshot(&self, snapshot: &TaskSnapshot) -> Result<(), RecoveryError> {
        ensure_supported_snapshot(snapshot)?;
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.check_no_active_reservation()?;
        self.save_task_snapshot_locked(snapshot)
    }

    /// 撤销删除等显式恢复路径:使用 max(tombstone,disk)+1 写入,
    /// 仅在 strict durable 写成功后才清除 tombstone。
    pub fn restore_task_snapshot(&self, snapshot: &TaskSnapshot) -> Result<(), RecoveryError> {
        ensure_supported_snapshot(snapshot)?;
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.check_no_active_reservation()?;
        self.restore_task_snapshot_locked(snapshot)
    }

    /// S-02b §3.3:restore 的 locked 实现。
    ///
    /// 关键不变式:
    /// 1. 使用 `max(tombstone_revision, disk_revision)+1` 作为写入 revision,
    ///    不得信任传入 snapshot.revision。
    /// 2. 仅在 strict durable write(`put_durable`)成功后才清除 tombstone;
    ///    写失败时 tombstone 保留,旧 revision save 仍被拒绝。
    fn restore_task_snapshot_locked(&self, snapshot: &TaskSnapshot) -> Result<(), RecoveryError> {
        let key = format!("task_{}", snapshot.id);
        // 先读取并分类磁盘原始快照,future/corrupt/I/O 均不得继续。
        let existing = self
            .load_task_snapshot_by_key(&key)
            .map_err(|error| error.into_recovery_error(&key))?;
        let disk_rev = existing.as_ref().map(|s| s.revision).unwrap_or(0);

        // tombstone revision:restore 路径需要取 max(tombstone, disk)+1
        let tomb_rev = self
            .delete_tombstones
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&snapshot.id)
            .copied()
            .unwrap_or(0);
        let write_rev = tomb_rev.max(disk_rev).saturating_add(1);

        let mut to_write = snapshot.clone();
        to_write.revision = write_rev;
        if to_write.schema_version < SNAPSHOT_SCHEMA_VERSION {
            to_write.schema_version = SNAPSHOT_SCHEMA_VERSION;
        }

        // strict durable write:成功后才清 tombstone
        self.store
            .put_durable(&key, &to_write)
            .map_err(RecoveryError::Io)?;

        // durable 写成功,清除 tombstone
        if let Ok(mut tombs) = self.delete_tombstones.lock() {
            tombs.remove(&snapshot.id);
        }
        Ok(())
    }

    fn save_task_snapshot_locked(&self, snapshot: &TaskSnapshot) -> Result<(), RecoveryError> {
        ensure_supported_snapshot(snapshot)?;
        let key = format!("task_{}", snapshot.id);
        // 先分类现有 raw，不能让 tombstone 提前掩盖 future/corrupt 快照。
        let existing = self
            .load_task_snapshot_by_key(&key)
            .map_err(|error| error.into_recovery_error(&key))?;

        // tombstone:拒绝基于删除前状态的旧写
        if let Ok(tombs) = self.delete_tombstones.lock()
            && let Some(&tomb_rev) = tombs.get(&snapshot.id)
            && snapshot.revision <= tomb_rev
        {
            tracing::warn!(
                task_id = %snapshot.id,
                incoming_revision = snapshot.revision,
                tombstone_revision = tomb_rev,
                "拒绝写入已删除任务快照(H-05 tombstone)"
            );
            return Ok(());
        }

        let base_rev = existing.as_ref().map(|s| s.revision).unwrap_or(0);
        if snapshot.revision < base_rev {
            tracing::warn!(
                task_id = %snapshot.id,
                incoming_revision = snapshot.revision,
                disk_revision = base_rev,
                "拒绝过期快照写入(H-05 revision CAS)"
            );
            return Ok(());
        }

        let mut to_write = snapshot.clone();
        to_write.revision = base_rev.saturating_add(1);
        if to_write.schema_version < SNAPSHOT_SCHEMA_VERSION {
            to_write.schema_version = SNAPSHOT_SCHEMA_VERSION;
        }
        self.store
            .put_durable(&key, &to_write)
            .map_err(RecoveryError::Io)
    }

    /// 加载任务快照
    ///
    /// S-02b:活跃 reservation 期间返回 `ReservationActive`。
    pub fn load_task_snapshot(&self, task_id: &str) -> Result<Option<TaskSnapshot>, RecoveryError> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.check_no_active_reservation()?;
        let key = format!("task_{task_id}");
        self.load_task_snapshot_by_key(&key)
            .map_err(|error| error.into_recovery_error(&key))
    }

    fn load_task_snapshot_by_key(
        &self,
        key: &str,
    ) -> Result<Option<TaskSnapshot>, LoadTaskSnapshotError> {
        let Some(json) = self.store.get_raw(key)? else {
            return Ok(None);
        };

        match classify_snapshot_schema(&json).map_err(|_| LoadTaskSnapshotError::Corrupt)? {
            SnapshotSchemaHeader::Version(found_version)
                if found_version > SNAPSHOT_SCHEMA_VERSION =>
            {
                Err(LoadTaskSnapshotError::Unsupported { found_version })
            }
            SnapshotSchemaHeader::Legacy | SnapshotSchemaHeader::Version(_) => {
                serde_json::from_str::<TaskSnapshot>(&json)
                    .or_else(|_| serde_json::from_str::<TaskRecord>(&json).map(TaskSnapshot::from))
                    .map(Some)
                    .map_err(|_| LoadTaskSnapshotError::Corrupt)
            }
        }
    }

    /// 加载所有任务快照,隔离损坏记录
    ///
    /// 单个 key 解析失败不会中断其他任务的恢复,而是记录到 `corrupt_keys` 中。
    pub fn load_all_task_snapshots(&self) -> Result<RecoveryResult, RecoveryError> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.check_no_active_reservation()?;
        let mut tasks = Vec::new();
        let mut corrupt_keys = Vec::new();
        let mut unsupported_schema = Vec::new();
        for key in self.store.keys().map_err(RecoveryError::Io)? {
            if key.starts_with("task_") {
                match self.load_task_snapshot_by_key(&key) {
                    Ok(Some(snapshot)) => tasks.push(snapshot),
                    Ok(None) => {} // key 存在但无数据,忽略
                    Err(LoadTaskSnapshotError::Unsupported { found_version }) => {
                        tracing::warn!(key = %key, found_version, "future schema 快照已隔离");
                        unsupported_schema.push(protected_snapshot(&key, found_version));
                    }
                    Err(LoadTaskSnapshotError::Corrupt) => {
                        tracing::warn!(key = %key, "快照 JSON 损坏,跳过恢复");
                        corrupt_keys.push(key);
                    }
                    Err(LoadTaskSnapshotError::Io(error)) => return Err(RecoveryError::Io(error)),
                }
            }
        }
        Ok(RecoveryResult {
            tasks,
            corrupt_keys,
            unsupported_schema,
        })
    }

    /// 保存任务记录（旧接口）
    pub fn save_task(&self, record: &TaskRecord) -> Result<(), RecoveryError> {
        let snapshot: TaskSnapshot = TaskSnapshot::from(record.clone());
        self.save_task_snapshot(&snapshot)
    }

    /// 加载任务记录（旧接口）
    pub fn load_task(&self, task_id: &str) -> Result<Option<TaskRecord>, RecoveryError> {
        Ok(self.load_task_snapshot(task_id)?.map(TaskRecord::from))
    }
    /// 删除任务记录(审计 H-05:持锁 + tombstone 防旧 save 复活)
    pub fn remove_task(&self, task_id: &str) -> Result<bool, RecoveryError> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.check_no_active_reservation()?;
        let key = format!("task_{task_id}");
        let existing_rev = self
            .load_task_snapshot_by_key(&key)
            .map_err(|error| error.into_recovery_error(&key))?
            .map(|s| s.revision)
            .unwrap_or(0);
        let deleted = self.store.delete(&key).map_err(RecoveryError::Io)?;
        if let Ok(mut tombs) = self.delete_tombstones.lock() {
            // 即便 key 本就不存在,也记 tombstone,挡住 in-flight 的旧 full-save
            tombs.insert(task_id.to_string(), existing_rev);
        }
        Ok(deleted)
    }

    /// 恢复所有未完成的任务（新接口）,隔离损坏记录
    ///
    /// 单个 key 解析失败不会中断恢复,而是记录到 `corrupt_keys` 中。
    pub fn recover_pending_snapshots(&self) -> Result<RecoveryResult, RecoveryError> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.check_no_active_reservation()?;
        let mut tasks = Vec::new();
        let mut corrupt_keys = Vec::new();
        let mut unsupported_schema = Vec::new();
        for key in self.store.keys().map_err(RecoveryError::Io)? {
            if key.starts_with("task_") {
                match self.load_task_snapshot_by_key(&key) {
                    Ok(Some(snapshot))
                        if matches!(
                            snapshot.status,
                            tachyon_core::DownloadState::Downloading
                                | tachyon_core::DownloadState::Paused
                        ) =>
                    {
                        tracing::info!(task_id = %snapshot.id, "恢复下载任务");
                        tasks.push(snapshot);
                    }
                    Ok(_) => {} // 完成或空,跳过
                    Err(LoadTaskSnapshotError::Unsupported { found_version }) => {
                        tracing::warn!(key = %key, found_version, "future schema 快照已隔离");
                        unsupported_schema.push(protected_snapshot(&key, found_version));
                    }
                    Err(LoadTaskSnapshotError::Corrupt) => {
                        tracing::warn!(key = %key, "快照 JSON 损坏,跳过恢复");
                        corrupt_keys.push(key);
                    }
                    Err(LoadTaskSnapshotError::Io(error)) => return Err(RecoveryError::Io(error)),
                }
            }
        }
        Ok(RecoveryResult {
            tasks,
            corrupt_keys,
            unsupported_schema,
        })
    }

    /// 原子性地读取-修改-写入任务快照
    ///
    /// 内部持有 `progress_lock` 确保 load-modify-save 序列的原子性,
    /// 防止并发分片进度更新之间的覆盖竞态。
    ///
    /// # 参数
    /// - `task_id`: 任务 ID
    /// - `patch`: 闭包,接收可变引用到快照,在锁内执行修改
    ///
    /// # 返回
    /// - `Ok(Some(TaskSnapshot))`: 快照存在且已更新
    /// - `Ok(None)`: 快照不存在
    /// - `Err`: I/O 或序列化错误
    pub fn update_snapshot(
        &self,
        task_id: &str,
        patch: impl FnOnce(&mut TaskSnapshot),
    ) -> Result<Option<TaskSnapshot>, RecoveryError> {
        let _lock = self.progress_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.check_no_active_reservation()?;
        let key = format!("task_{task_id}");
        let mut snapshot = match self
            .load_task_snapshot_by_key(&key)
            .map_err(|error| error.into_recovery_error(&key))?
        {
            Some(s) => s,
            None => return Ok(None),
        };

        patch(&mut snapshot);

        // 确保新写入的快照使用当前 schema 版本
        if snapshot.schema_version < SNAPSHOT_SCHEMA_VERSION {
            snapshot.schema_version = SNAPSHOT_SCHEMA_VERSION;
        }

        // 已持 progress_lock,走 locked save(revision CAS + bump)
        self.save_task_snapshot_locked(&snapshot)?;
        // 返回磁盘最终 revision
        let final_snap = self
            .load_task_snapshot_by_key(&key)
            .map_err(|error| error.into_recovery_error(&key))?;
        Ok(final_snap)
    }
}

/// S-02b:Drop 时释放匹配的活跃 reservation。
///
/// 仅当当前 active reservation 的 nonce 与本 reservation 匹配时才清除;
/// 若已被其他 reservation 替换(理论上不应发生,因 progress_lock 串行化),
/// 则不修改。
impl Drop for TaskNamespaceReservation<'_> {
    fn drop(&mut self) {
        let _lock = self
            .manager
            .progress_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Ok(mut active) = self.manager.active_reservation.lock() {
            if active.as_ref() == Some(&self.nonce) {
                *active = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_record(task_id: &str, status: &str) -> TaskRecord {
        TaskRecord {
            task_id: task_id.to_string(),
            url: format!("https://example.com/{task_id}.zip"),
            save_path: format!("/downloads/{task_id}.zip"),
            file_size: Some(1024),
            downloaded: 512,
            completed_fragments: vec![0, 1],
            total_fragments: 4,
            status: status.to_string(),
        }
    }

    fn make_snapshot(id: &str, status: tachyon_core::DownloadState) -> TaskSnapshot {
        TaskSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: id.to_string(),
            url: format!("https://example.com/{id}.zip"),
            save_path: format!("/downloads/{id}.zip"),
            file_name: format!("{id}.zip"),
            file_size: Some(1024),
            downloaded: 512,
            completed_fragments: vec![0, 1],
            partial_fragments: HashMap::new(),
            total_fragments: 4,
            fragment_size: 256,
            status,
            etag: None,
            last_modified: None,
            content_length: Some(1024),
            supports_range: true,
            created_at: String::new(),
            updated_at: String::new(),
            fail_reason: None,
            retry_count: 0,
            tags: vec![],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        }
    }

    // ── TaskRecord 旧接口测试 ──

    #[test]
    fn test_save_and_load_task() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let record = make_record("task-1", "downloading");
        mgr.save_task(&record).unwrap();
        let loaded = mgr.load_task("task-1").unwrap().unwrap();
        assert_eq!(loaded.task_id, "task-1");
        assert_eq!(loaded.downloaded, 512);
    }

    #[test]
    fn test_recover_pending_snapshots() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        mgr.save_task(&make_record("t1", "downloading")).unwrap();
        mgr.save_task(&make_record("t2", "completed")).unwrap();
        mgr.save_task(&make_record("t3", "paused")).unwrap();
        mgr.save_task(&make_record("t4", "failed")).unwrap();

        let result = mgr.recover_pending_snapshots().unwrap();
        assert_eq!(result.tasks.len(), 2);
        assert!(result.corrupt_keys.is_empty());
        assert!(result.unsupported_schema.is_empty());
        let ids: Vec<&str> = result
            .tasks
            .iter()
            .map(|snapshot| snapshot.id.as_str())
            .collect();
        assert!(ids.contains(&"t1"));
        assert!(ids.contains(&"t3"));
    }

    #[test]
    fn test_remove_task() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        mgr.save_task(&make_record("t1", "completed")).unwrap();
        assert!(mgr.remove_task("t1").unwrap());
        assert!(mgr.load_task("t1").unwrap().is_none());
    }

    #[test]
    fn test_load_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        assert!(mgr.load_task("no-such-task").unwrap().is_none());
    }

    // ── TaskSnapshot 新接口测试 ──

    #[test]
    fn snapshot_save_and_load() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let snap = make_snapshot("s1", tachyon_core::DownloadState::Downloading);
        mgr.save_task_snapshot(&snap).unwrap();
        let loaded = mgr.load_task_snapshot("s1").unwrap().unwrap();
        // save 会 bump revision:0 -> 1
        let mut expected = snap;
        expected.revision = 1;
        assert_eq!(loaded, expected);
    }

    #[test]
    fn snapshot_load_all() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        mgr.save_task_snapshot(&make_snapshot(
            "a",
            tachyon_core::DownloadState::Downloading,
        ))
        .unwrap();
        mgr.save_task_snapshot(&make_snapshot("b", tachyon_core::DownloadState::Completed))
            .unwrap();
        mgr.save_task_snapshot(&make_snapshot("c", tachyon_core::DownloadState::Paused))
            .unwrap();

        let result = mgr.load_all_task_snapshots().unwrap();
        assert_eq!(result.tasks.len(), 3);
        assert!(result.corrupt_keys.is_empty());
        let ids: Vec<&str> = result.tasks.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
        assert!(ids.contains(&"c"));
    }

    #[test]
    fn snapshot_recover_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        mgr.save_task_snapshot(&make_snapshot(
            "p1",
            tachyon_core::DownloadState::Downloading,
        ))
        .unwrap();
        mgr.save_task_snapshot(&make_snapshot("p2", tachyon_core::DownloadState::Completed))
            .unwrap();
        mgr.save_task_snapshot(&make_snapshot("p3", tachyon_core::DownloadState::Paused))
            .unwrap();
        mgr.save_task_snapshot(&make_snapshot("p4", tachyon_core::DownloadState::Failed))
            .unwrap();

        let result = mgr.recover_pending_snapshots().unwrap();
        assert_eq!(result.tasks.len(), 2);
        assert!(result.corrupt_keys.is_empty());
        let ids: Vec<&str> = result.tasks.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"p1"));
        assert!(ids.contains(&"p3"));
    }

    #[test]
    fn snapshot_load_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        assert!(mgr.load_task_snapshot("ghost").unwrap().is_none());
    }

    #[test]
    fn snapshot_to_record_conversion() {
        let snap = make_snapshot("conv", tachyon_core::DownloadState::Downloading);
        let record: TaskRecord = snap.clone().into();
        assert_eq!(record.task_id, "conv");
        assert_eq!(record.completed_fragments, vec![0, 1]);
        assert_eq!(record.status, "downloading");
    }

    // ── 边界条件 ──

    #[test]
    fn snapshot_empty_fragments() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let snap = make_snapshot("empty", tachyon_core::DownloadState::Downloading);
        mgr.save_task_snapshot(&snap).unwrap();
        let loaded = mgr.load_task_snapshot("empty").unwrap().unwrap();
        let mut expected = snap;
        expected.revision = 1;
        assert_eq!(loaded, expected);
    }

    #[test]
    fn snapshot_recovers_legacy_task_record_json() {
        let tmp = tempfile::tempdir().unwrap();
        let raw_json = r#"{
            "task_id":"legacy-1",
            "url":"https://example.com/legacy.bin",
            "save_path":"/downloads/legacy.bin",
            "file_size":1024,
            "downloaded":512,
            "completed_fragments":[0,1],
            "total_fragments":4,
            "status":"paused"
        }"#;
        std::fs::write(tmp.path().join("task_legacy_1.json"), raw_json).unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let result = mgr.recover_pending_snapshots().unwrap();

        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].id, "legacy-1");
        assert_eq!(result.tasks[0].file_name, "legacy.bin");
        assert_eq!(result.tasks[0].status, tachyon_core::DownloadState::Paused);
    }

    #[test]
    fn test_task_snapshot_serializes_typed_status_and_metadata() {
        let snapshot = TaskSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            revision: 0,
            id: "task-1".to_string(),
            url: "https://example.com/file.bin".to_string(),
            save_path: "/downloads/file.bin".to_string(),
            file_name: "file.bin".to_string(),
            file_size: Some(1024),
            downloaded: 512,
            completed_fragments: vec![0, 1],
            partial_fragments: HashMap::new(),
            total_fragments: 4,
            fragment_size: 256,
            status: tachyon_core::DownloadState::Paused,
            etag: Some("\"abc\"".to_string()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_string()),
            content_length: Some(1024),
            supports_range: true,
            created_at: "2026-05-29T00:00:00Z".to_string(),
            updated_at: "2026-05-29T00:00:01Z".to_string(),
            fail_reason: None,
            retry_count: 0,
            tags: vec!["model".to_string(), "important".to_string()],
            hf_meta: None,
            display_order: 0,
            mirror_urls: None,
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(json.contains("paused"));
        let loaded: TaskSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.status, tachyon_core::DownloadState::Paused);
        assert_eq!(loaded.completed_fragments, vec![0, 1]);
        assert_eq!(loaded.etag.as_deref(), Some("\"abc\""));
    }

    // ── schema_version 兼容性测试 ──

    #[test]
    fn snapshot_old_json_defaults_supports_range_true() {
        // 缺 supportsRange 字段的旧 JSON 默认 true(可分片续传)
        let old_json = r#"{
            "id":"t1","url":"https://e.com/a","savePath":"/a","fileName":"a",
            "fileSize":1,"downloaded":0,"completedFragments":[],"totalFragments":1,
            "fragmentSize":1,"status":"paused","createdAt":"","updatedAt":""
        }"#;
        let snapshot: TaskSnapshot = serde_json::from_str(old_json).unwrap();
        assert!(
            snapshot.supports_range,
            "旧快照缺字段时 supports_range 默认 true"
        );
    }

    #[test]
    fn snapshot_old_json_without_schema_version_deserializes() {
        // 模拟旧版本 JSON(无 schemaVersion 字段)
        let old_json = r#"{
            "id":"old-task",
            "url":"https://example.com/old.bin",
            "savePath":"/downloads/old.bin",
            "fileName":"old.bin",
            "fileSize":2048,
            "downloaded":512,
            "completedFragments":[0],
            "totalFragments":4,
            "fragmentSize":512,
            "status":"downloading",
            "createdAt":"2026-01-01T00:00:00Z",
            "updatedAt":"2026-01-01T00:00:01Z",
            "retryCount":0
        }"#;
        let snapshot: TaskSnapshot = serde_json::from_str(old_json).unwrap();
        // 旧 JSON 无 schemaVersion,应默认为 0
        assert_eq!(snapshot.schema_version, 0);
        assert_eq!(snapshot.id, "old-task");
        assert_eq!(snapshot.downloaded, 512);
        // 旧 JSON 无 displayOrder,应默认为 0
        assert_eq!(snapshot.display_order, 0);
    }

    #[test]
    fn snapshot_new_json_with_schema_version_deserializes() {
        let new_json = r#"{
            "schemaVersion":1,
            "id":"new-task",
            "url":"https://example.com/new.bin",
            "savePath":"/downloads/new.bin",
            "fileName":"new.bin",
            "fileSize":4096,
            "downloaded":1024,
            "completedFragments":[0,1],
            "totalFragments":8,
            "fragmentSize":512,
            "status":"paused",
            "createdAt":"2026-06-01T00:00:00Z",
            "updatedAt":"2026-06-01T00:00:01Z",
            "retryCount":1
        }"#;
        let snapshot: TaskSnapshot = serde_json::from_str(new_json).unwrap();
        assert_eq!(snapshot.schema_version, 1);
        assert_eq!(snapshot.id, "new-task");
    }

    // ── mirror_urls 快照持久化(schema v7) ──

    #[test]
    fn test_task_snapshot_mirror_urls_json_roundtrip() {
        let mut snapshot = make_snapshot("mirror-task", tachyon_core::DownloadState::Paused);
        snapshot.mirror_urls = Some(vec![
            "https://mirror1.example.com/file.bin".to_string(),
            "https://mirror2.example.com/file.bin".to_string(),
        ]);

        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(
            json.contains("mirrorUrls"),
            "序列化应使用 camelCase mirrorUrls: {json}"
        );
        let loaded: TaskSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(
            loaded.mirror_urls.as_deref(),
            Some(
                [
                    "https://mirror1.example.com/file.bin".to_string(),
                    "https://mirror2.example.com/file.bin".to_string(),
                ]
                .as_slice()
            )
        );
        assert_eq!(loaded.schema_version, SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(SNAPSHOT_SCHEMA_VERSION, 7);
    }

    #[test]
    fn test_task_snapshot_old_json_without_mirror_urls_deserializes_to_none() {
        // 旧版快照无 mirrorUrls 字段时必须默认为 None,不能反序列化失败
        let old_json = r#"{
            "schemaVersion":6,
            "id":"legacy-mirror",
            "url":"https://example.com/legacy.bin",
            "savePath":"/downloads/legacy.bin",
            "fileName":"legacy.bin",
            "fileSize":2048,
            "downloaded":512,
            "completedFragments":[0],
            "totalFragments":4,
            "fragmentSize":512,
            "status":"paused",
            "createdAt":"2026-01-01T00:00:00Z",
            "updatedAt":"2026-01-01T00:00:01Z",
            "retryCount":0
        }"#;
        let snapshot: TaskSnapshot = serde_json::from_str(old_json).unwrap();
        assert!(
            snapshot.mirror_urls.is_none(),
            "旧 JSON 无 mirrorUrls 时应为 None"
        );
    }

    #[test]
    fn test_task_snapshot_kv_roundtrip_preserves_mirror_urls() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let mut snapshot = make_snapshot("kv-mirrors", tachyon_core::DownloadState::Downloading);
        snapshot.mirror_urls = Some(vec!["https://cdn.example.com/a.bin".to_string()]);
        mgr.save_task_snapshot(&snapshot).unwrap();

        let loaded = mgr.load_task_snapshot("kv-mirrors").unwrap().unwrap();
        assert_eq!(
            loaded.mirror_urls,
            Some(vec!["https://cdn.example.com/a.bin".to_string()])
        );
    }

    // ── 坏 JSON 隔离测试 ──

    fn task_key_path(dir: &std::path::Path, task_id: &str) -> std::path::PathBuf {
        dir.join(format!("task_{task_id}.json"))
    }

    fn write_raw_task(dir: &std::path::Path, task_id: &str, raw: &str) -> std::path::PathBuf {
        let path = task_key_path(dir, task_id);
        std::fs::write(&path, raw).unwrap();
        path
    }

    fn future_legacy_record_raw(task_id: &str, found_version: u32) -> String {
        let legacy_record_json =
            serde_json::to_string(&make_record(task_id, "downloading")).unwrap();
        format!(
            r#"{{"schemaVersion":{found_version},{}"#,
            legacy_record_json.trim_start_matches('{')
        )
    }

    fn assert_unsupported(error: RecoveryError, key: &str, found_version: u32) {
        match error {
            RecoveryError::Unsupported(protected) => {
                assert_eq!(protected.key, key);
                assert_eq!(protected.found_version, found_version);
                assert_eq!(protected.supported_version, SNAPSHOT_SCHEMA_VERSION);
            }
            other => panic!("expected typed Unsupported for {key}, got {other:?}"),
        }
    }

    fn assert_invalid_data(error: RecoveryError, key: &str) {
        match error {
            RecoveryError::InvalidData { key: actual_key } => assert_eq!(actual_key, key),
            other => panic!("expected typed InvalidData for {key}, got {other:?}"),
        }
    }

    #[test]
    fn future_schema_single_load_returns_typed_protected_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let found_version = SNAPSHOT_SCHEMA_VERSION + 1;
        let raw = future_legacy_record_raw("future", found_version);
        assert!(serde_json::from_str::<TaskRecord>(&raw).is_ok());
        let path = write_raw_task(tmp.path(), "future", &raw);
        let raw_before = std::fs::read(&path).unwrap();

        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        assert_unsupported(
            mgr.load_task_snapshot("future").unwrap_err(),
            "task_future",
            found_version,
        );
        assert_eq!(std::fs::read(path).unwrap(), raw_before);
    }

    #[test]
    fn invalid_schema_headers_return_typed_invalid_data_without_legacy_fallback() {
        let cases = [
            (
                "duplicate",
                r#"{"schemaVersion":7,"schemaVersion":8,"task_id":"duplicate","url":"https://example.com/duplicate.zip","save_path":"/downloads/duplicate.zip","file_size":1,"downloaded":0,"completed_fragments":[],"total_fragments":1,"status":"paused"}"#,
            ),
            (
                "null",
                r#"{"schemaVersion":null,"task_id":"null","url":"https://example.com/null.zip","save_path":"/downloads/null.zip","file_size":1,"downloaded":0,"completed_fragments":[],"total_fragments":1,"status":"paused"}"#,
            ),
            (
                "string",
                r#"{"schemaVersion":"8","task_id":"string","url":"https://example.com/string.zip","save_path":"/downloads/string.zip","file_size":1,"downloaded":0,"completed_fragments":[],"total_fragments":1,"status":"paused"}"#,
            ),
            (
                "negative",
                r#"{"schemaVersion":-1,"task_id":"negative","url":"https://example.com/negative.zip","save_path":"/downloads/negative.zip","file_size":1,"downloaded":0,"completed_fragments":[],"total_fragments":1,"status":"paused"}"#,
            ),
            (
                "overflow",
                r#"{"schemaVersion":4294967296,"task_id":"overflow","url":"https://example.com/overflow.zip","save_path":"/downloads/overflow.zip","file_size":1,"downloaded":0,"completed_fragments":[],"total_fragments":1,"status":"paused"}"#,
            ),
            ("non_object", r#"["not", "a", "snapshot"]"#),
            ("trailing", r#"{"schemaVersion":7} {"task_id":"trailing"}"#),
        ];

        for (task_id, raw) in cases {
            let tmp = tempfile::tempdir().unwrap();
            assert!(
                matches!(task_id, "non_object" | "trailing")
                    || serde_json::from_str::<TaskRecord>(raw).is_ok(),
                "fixture {task_id} must be a viable legacy fallback decoy when it is an object"
            );
            let path = write_raw_task(tmp.path(), task_id, raw);
            let raw_before = std::fs::read(&path).unwrap();
            let store = KvStore::open(tmp.path()).unwrap();
            let mgr = RecoveryManager::new(store);

            assert_invalid_data(
                mgr.load_task_snapshot(task_id).unwrap_err(),
                &format!("task_{task_id}"),
            );
            assert_eq!(std::fs::read(path).unwrap(), raw_before, "{task_id}");
        }
    }

    #[test]
    fn future_incoming_save_restore_and_remove_preserve_state() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let found_version = SNAPSHOT_SCHEMA_VERSION + 1;

        // future incoming snapshot 必须在碰触磁盘或 tombstone 前被拒绝。
        let mut incoming = make_snapshot("incoming", tachyon_core::DownloadState::Paused);
        incoming.schema_version = found_version;
        mgr.delete_tombstones
            .lock()
            .unwrap()
            .insert("incoming".to_string(), 41);
        assert_unsupported(
            mgr.save_task_snapshot(&incoming).unwrap_err(),
            "task_incoming",
            found_version,
        );
        assert_unsupported(
            mgr.restore_task_snapshot(&incoming).unwrap_err(),
            "task_incoming",
            found_version,
        );
        assert!(!task_key_path(tmp.path(), "incoming").exists());
        assert_eq!(
            mgr.delete_tombstones.lock().unwrap().get("incoming"),
            Some(&41)
        );

        // current incoming 也不能覆盖、restore 或删除磁盘上已有的 future raw。
        let protected_raw = future_legacy_record_raw("protected", found_version);
        let protected_path = write_raw_task(tmp.path(), "protected", &protected_raw);
        let protected_before = std::fs::read(&protected_path).unwrap();
        mgr.delete_tombstones
            .lock()
            .unwrap()
            .insert("protected".to_string(), 73);
        let current = make_snapshot("protected", tachyon_core::DownloadState::Paused);

        assert_unsupported(
            mgr.save_task_snapshot(&current).unwrap_err(),
            "task_protected",
            found_version,
        );
        assert_unsupported(
            mgr.restore_task_snapshot(&current).unwrap_err(),
            "task_protected",
            found_version,
        );
        assert_unsupported(
            mgr.remove_task("protected").unwrap_err(),
            "task_protected",
            found_version,
        );
        assert_eq!(std::fs::read(protected_path).unwrap(), protected_before);
        assert_eq!(
            mgr.delete_tombstones.lock().unwrap().get("protected"),
            Some(&73)
        );
    }

    #[test]
    fn update_snapshot_rejects_patch_that_creates_future_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let current = make_snapshot("current", tachyon_core::DownloadState::Downloading);
        mgr.save_task_snapshot(&current).unwrap();
        let current_path = task_key_path(tmp.path(), "current");
        let current_before = std::fs::read(&current_path).unwrap();
        let current_revision = mgr.load_task_snapshot("current").unwrap().unwrap().revision;
        let current_patch_called = std::cell::Cell::new(false);

        assert_unsupported(
            mgr.update_snapshot("current", |snapshot| {
                current_patch_called.set(true);
                snapshot.schema_version = SNAPSHOT_SCHEMA_VERSION + 1;
            })
            .unwrap_err(),
            "task_current",
            SNAPSHOT_SCHEMA_VERSION + 1,
        );
        assert!(
            current_patch_called.get(),
            "current raw must invoke the patch before rejecting its future schema"
        );
        assert_eq!(std::fs::read(&current_path).unwrap(), current_before);
        assert_eq!(
            mgr.load_task_snapshot("current").unwrap().unwrap().revision,
            current_revision
        );

        let future_version = SNAPSHOT_SCHEMA_VERSION + 1;
        let future_raw = future_legacy_record_raw("direct_future", future_version);
        let future_path = write_raw_task(tmp.path(), "direct_future", &future_raw);
        let future_before = std::fs::read(&future_path).unwrap();
        let patch_called = std::cell::Cell::new(false);
        assert_unsupported(
            mgr.update_snapshot("direct_future", |_| patch_called.set(true))
                .unwrap_err(),
            "task_direct_future",
            future_version,
        );
        assert!(
            !patch_called.get(),
            "future raw must not invoke the patch closure"
        );
        assert_eq!(std::fs::read(future_path).unwrap(), future_before);
    }

    #[test]
    fn recover_pending_snapshots_keeps_legal_tasks_and_classifies_future_and_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        mgr.save_task_snapshot(&make_snapshot(
            "pending",
            tachyon_core::DownloadState::Downloading,
        ))
        .unwrap();
        mgr.save_task_snapshot(&make_snapshot(
            "completed",
            tachyon_core::DownloadState::Completed,
        ))
        .unwrap();

        let future_raw = future_legacy_record_raw("future", SNAPSHOT_SCHEMA_VERSION + 1);
        let future_path = write_raw_task(tmp.path(), "future", &future_raw);
        let future_before = std::fs::read(&future_path).unwrap();
        let corrupt_path = write_raw_task(tmp.path(), "corrupt", r#"{"schemaVersion":null}"#);
        let corrupt_before = std::fs::read(&corrupt_path).unwrap();

        let result = mgr.recover_pending_snapshots().unwrap();
        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].id, "pending");
        assert_eq!(result.corrupt_keys, vec!["task_corrupt"]);
        assert_eq!(
            result.unsupported_schema,
            vec![ProtectedSnapshot {
                key: "task_future".to_string(),
                found_version: SNAPSHOT_SCHEMA_VERSION + 1,
                supported_version: SNAPSHOT_SCHEMA_VERSION,
            }]
        );
        assert_eq!(std::fs::read(future_path).unwrap(), future_before);
        assert_eq!(std::fs::read(corrupt_path).unwrap(), corrupt_before);
    }

    #[test]
    fn future_schema_is_reported_as_unsupported_not_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let current_path = tmp.path().join("task_current.json");
        let future_path = tmp.path().join("task_future.json");
        let found_version = SNAPSHOT_SCHEMA_VERSION + 1;

        let current = make_snapshot("current", tachyon_core::DownloadState::Downloading);
        std::fs::write(&current_path, serde_json::to_vec(&current).unwrap()).unwrap();

        // future JSON 的其余字段刻意保持为合法 TaskRecord：若 schema 检查晚于
        // TaskRecord fallback，这条记录会被错误地当作旧记录恢复。
        let legacy_record_json =
            serde_json::to_string(&make_record("future", "downloading")).unwrap();
        let future_raw = format!(
            r#"{{"schemaVersion":{found_version},{}"#,
            legacy_record_json.trim_start_matches('{')
        );
        assert!(future_raw.contains(&format!(r#""schemaVersion":{found_version}"#)));
        assert!(serde_json::from_str::<TaskRecord>(&future_raw).is_ok());
        std::fs::write(&future_path, future_raw.as_bytes()).unwrap();
        let future_raw_before = std::fs::read(&future_path).unwrap();

        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let result = mgr.load_all_task_snapshots().unwrap();

        assert!(result.tasks.iter().any(|snapshot| snapshot.id == "current"));
        assert!(result.tasks.iter().all(|snapshot| snapshot.id != "future"));
        assert!(!result.corrupt_keys.iter().any(|key| key == "task_future"));
        assert_eq!(result.unsupported_schema.len(), 1);
        let protected = &result.unsupported_schema[0];
        assert_eq!(protected.key, "task_future");
        assert_eq!(protected.found_version, found_version);
        assert_eq!(protected.supported_version, SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(std::fs::read(&future_path).unwrap(), future_raw_before);
    }

    #[test]
    fn corrupt_json_is_isolated_during_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        // 保存一个正常任务
        mgr.save_task_snapshot(&make_snapshot(
            "good",
            tachyon_core::DownloadState::Downloading,
        ))
        .unwrap();

        // 直接写入一个损坏的 JSON 文件
        let corrupt_path = tmp.path().join("task_corrupt.json");
        std::fs::write(&corrupt_path, "{ this is not valid json !!!").unwrap();

        // 恢复不应失败,应返回正常任务并标记损坏 key
        let result = mgr.recover_pending_snapshots().unwrap();
        assert_eq!(result.tasks.len(), 1);
        assert_eq!(result.tasks[0].id, "good");
        assert_eq!(result.corrupt_keys.len(), 1);
        assert!(result.corrupt_keys[0].contains("corrupt"));
    }

    #[test]
    fn corrupt_json_is_isolated_in_load_all() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        mgr.save_task_snapshot(&make_snapshot(
            "ok1",
            tachyon_core::DownloadState::Completed,
        ))
        .unwrap();
        mgr.save_task_snapshot(&make_snapshot("ok2", tachyon_core::DownloadState::Paused))
            .unwrap();

        let corrupt_path = tmp.path().join("task_bad.json");
        std::fs::write(&corrupt_path, "not json at all").unwrap();

        let result = mgr.load_all_task_snapshots().unwrap();
        assert_eq!(result.tasks.len(), 2);
        assert_eq!(result.corrupt_keys.len(), 1);
    }

    // ── B7: RecoveryManager 强制 Durable 测试 ──

    /// B7: RecoveryManager 在 Fast store 上写入快照后,重新打开仍可恢复
    ///
    /// `save_task_snapshot` 必须走 `put_durable`(fsync),保证进程崩溃后进度不丢失。
    /// 此测试用 "关闭实例后重开" 模拟崩溃:若写入仅停留在 OS 页面缓存,
    /// 在真实断电场景会丢失;此处验证至少数据已正确落盘到文件系统可读状态。
    #[test]
    fn recovery_snapshot_survives_reopen_on_fast_store() {
        let tmp = tempfile::tempdir().unwrap();
        let snap = make_snapshot("crash", tachyon_core::DownloadState::Downloading);

        // 写入后关闭实例(模拟进程退出/崩溃)
        {
            let store = KvStore::open(tmp.path()).unwrap();
            assert_eq!(store.durability(), crate::Durability::Fast);
            let mgr = RecoveryManager::new(store);
            mgr.save_task_snapshot(&snap).unwrap();
        }

        // 重新打开,验证快照可恢复
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let loaded = mgr.load_task_snapshot("crash").unwrap().unwrap();
        let mut expected = snap;
        expected.revision = 1;
        assert_eq!(loaded, expected);
    }

    /// B7: `update_snapshot` 同样走 Durable 路径(经 save_task_snapshot),
    /// 重开后 patch 后的进度可恢复
    #[test]
    fn recovery_update_snapshot_survives_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let store = KvStore::open(tmp.path()).unwrap();
            let mgr = RecoveryManager::new(store);
            let snap = make_snapshot("up", tachyon_core::DownloadState::Downloading);
            mgr.save_task_snapshot(&snap).unwrap();
            mgr.update_snapshot("up", |s| {
                s.downloaded = 999;
                s.completed_fragments.push(2);
            })
            .unwrap();
        }

        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let loaded = mgr.load_task_snapshot("up").unwrap().unwrap();
        assert_eq!(loaded.downloaded, 999);
        assert!(loaded.completed_fragments.contains(&2));
    }

    // ── update_snapshot 原子性测试 ──

    #[test]
    fn update_snapshot_applies_patch_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let snap = make_snapshot("atomic", tachyon_core::DownloadState::Downloading);
        mgr.save_task_snapshot(&snap).unwrap();

        let updated = mgr
            .update_snapshot("atomic", |s| {
                s.downloaded = 768;
                s.completed_fragments.push(2);
            })
            .unwrap()
            .unwrap();

        assert_eq!(updated.downloaded, 768);
        assert!(updated.completed_fragments.contains(&2));
        assert_eq!(updated.schema_version, SNAPSHOT_SCHEMA_VERSION);

        // 验证持久化
        let loaded = mgr.load_task_snapshot("atomic").unwrap().unwrap();
        assert_eq!(loaded.downloaded, 768);
        assert!(loaded.completed_fragments.contains(&2));
    }

    #[test]
    fn update_snapshot_returns_none_for_missing_task() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let result = mgr.update_snapshot("nonexistent", |_| {}).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn update_snapshot_upgrades_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        // 直接写入旧 schema 版本的 JSON
        let mut snap = make_snapshot("old-schema", tachyon_core::DownloadState::Paused);
        snap.schema_version = 0;
        mgr.save_task_snapshot(&snap).unwrap();

        let updated = mgr.update_snapshot("old-schema", |_| {}).unwrap().unwrap();

        assert_eq!(updated.schema_version, SNAPSHOT_SCHEMA_VERSION);
    }

    #[test]
    fn test_h05_stale_full_save_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let mut paused = make_snapshot("h05", tachyon_core::DownloadState::Paused);
        mgr.save_task_snapshot(&paused).unwrap();
        let on_disk = mgr.load_task_snapshot("h05").unwrap().unwrap();
        assert_eq!(on_disk.revision, 1);
        assert_eq!(on_disk.status, tachyon_core::DownloadState::Paused);

        // 模拟较新 full-save(Downloading)先基于 rev1 写出
        let mut downloading = on_disk.clone();
        downloading.status = tachyon_core::DownloadState::Downloading;
        mgr.save_task_snapshot(&downloading).unwrap();
        let mid = mgr.load_task_snapshot("h05").unwrap().unwrap();
        assert_eq!(mid.revision, 2);
        assert_eq!(mid.status, tachyon_core::DownloadState::Downloading);

        // 旧的 Paused full-save(仍带 rev1)后到,必须拒绝
        paused.revision = 1;
        paused.status = tachyon_core::DownloadState::Paused;
        mgr.save_task_snapshot(&paused).unwrap();
        let final_snap = mgr.load_task_snapshot("h05").unwrap().unwrap();
        assert_eq!(final_snap.revision, 2);
        assert_eq!(
            final_snap.status,
            tachyon_core::DownloadState::Downloading,
            "过期 full-save 不得覆盖较新状态"
        );
    }

    #[test]
    fn test_h05_remove_then_stale_save_does_not_resurrect() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let snap = make_snapshot("gone", tachyon_core::DownloadState::Downloading);
        mgr.save_task_snapshot(&snap).unwrap();
        let on_disk = mgr.load_task_snapshot("gone").unwrap().unwrap();
        assert!(mgr.remove_task("gone").unwrap());
        assert!(mgr.load_task_snapshot("gone").unwrap().is_none());

        // 旧 in-flight save 带着删除前 revision,不得复活
        mgr.save_task_snapshot(&on_disk).unwrap();
        assert!(
            mgr.load_task_snapshot("gone").unwrap().is_none(),
            "删除后旧 save 不得复活快照"
        );
    }

    #[test]
    fn test_h05_restore_after_delete_clears_tombstone() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);

        let snap = make_snapshot("undo", tachyon_core::DownloadState::Paused);
        mgr.save_task_snapshot(&snap).unwrap();
        let on_disk = mgr.load_task_snapshot("undo").unwrap().unwrap();
        assert!(mgr.remove_task("undo").unwrap());
        mgr.restore_task_snapshot(&on_disk).unwrap();
        let restored = mgr.load_task_snapshot("undo").unwrap().unwrap();
        assert_eq!(restored.status, tachyon_core::DownloadState::Paused);
        assert!(restored.revision >= 1);
    }
    // ── S-02b: restore strict durable 顺序(§3.3)──

    /// §3.3 step 3:restore 必须使用 `max(tombstone, disk)+1` 作为写入 revision,
    /// 不得信任传入 revision;且仅在 strict durable 写成功后才清除 tombstone。
    ///
    /// 当前实现先清 tombstone 再以 `disk+1` 写入,既信任错误顺序又用错 revision,
    /// 故此测试为 RED:期望 revision == 6,实际为 2。
    #[test]
    fn s02b_restore_uses_max_of_tombstone_and_disk_revision_then_clears_tombstone() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let snap = make_snapshot("maxrev", tachyon_core::DownloadState::Paused);
        mgr.save_task_snapshot(&snap).unwrap();
        let disk = mgr.load_task_snapshot("maxrev").unwrap().unwrap();
        assert_eq!(disk.revision, 1);

        // tombstone revision(5)高于 disk revision(1)
        mgr.delete_tombstones
            .lock()
            .unwrap()
            .insert("maxrev".to_string(), 5);

        // incoming revision 与 disk 持平,确保走到 durable write 而非 CAS 拒绝;
        // spec 明确"不得信任传入 revision",故期望写入 max(5,1)+1 == 6。
        let mut incoming = snap.clone();
        incoming.revision = disk.revision;
        mgr.restore_task_snapshot(&incoming).unwrap();

        let restored = mgr.load_task_snapshot("maxrev").unwrap().unwrap();
        assert_eq!(
            restored.revision, 6,
            "restore 必须使用 max(tombstone,disk)+1,不得信任传入 revision"
        );
        assert!(
            mgr.delete_tombstones
                .lock()
                .unwrap()
                .get("maxrev")
                .is_none(),
            "tombstone 必须在 strict durable 写成功后才清除"
        );
    }

    /// §3.3 step 4-5:durable write 失败时 tombstone 必须保留,
    /// 且旧 revision save 仍被 tombstone 拒绝。
    ///
    /// 当前实现先清 tombstone 再写,故 write 失败后 tombstone 已丢失 → RED。
    /// 注入手段:将目标文件设为只读,使 `rename` 失败(读不受影响),
    /// 经探针确认 `load` 成功而 `put_durable` 返回 `RecoveryError::Io`。
    #[test]
    fn s02b_restore_keeps_tombstone_when_durable_write_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let snap = make_snapshot("rwfail", tachyon_core::DownloadState::Paused);
        mgr.save_task_snapshot(&snap).unwrap();
        let disk = mgr.load_task_snapshot("rwfail").unwrap().unwrap();
        assert_eq!(disk.revision, 1);

        // tombstone(5)高于 disk(1):若 restore 在写前清 tombstone,失败后即丢失。
        mgr.delete_tombstones
            .lock()
            .unwrap()
            .insert("rwfail".to_string(), 5);

        // 将目标文件设为只读,迫使 durable write 的 rename 失败。
        let path = task_key_path(tmp.path(), "rwfail");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();

        // incoming revision 与 disk 持平,确保走到 durable write 而非 CAS 拒绝。
        let mut incoming = snap.clone();
        incoming.revision = disk.revision;

        let err = mgr.restore_task_snapshot(&incoming).unwrap_err();
        match err {
            RecoveryError::Io(_) => {}
            other => {
                let mut p = std::fs::metadata(&path).unwrap().permissions();
                p.set_readonly(false);
                let _ = std::fs::set_permissions(&path, p);
                panic!("expected Io from failed durable write, got {other:?}");
            }
        }

        // 还原可写,便于后续断言与清理。
        let mut p = std::fs::metadata(&path).unwrap().permissions();
        p.set_readonly(false);
        let _ = std::fs::set_permissions(&path, p);

        // tombstone 必须保留:strict durable 写失败后不得清除。
        assert_eq!(
            mgr.delete_tombstones.lock().unwrap().get("rwfail"),
            Some(&5),
            "durable write 失败后 tombstone 必须保留(不得在写前清除)"
        );

        // 旧 revision save 仍被 tombstone 拒绝:tombstone(5)仍生效,
        // incoming.revision(1) <= 5 → CAS 拒绝,不得写入。
        mgr.save_task_snapshot(&incoming).unwrap();
        let after = mgr.load_task_snapshot("rwfail").unwrap().unwrap();
        assert_eq!(
            after.revision, disk.revision,
            "tombstone 仍生效,旧 revision save 不得写入"
        );
        assert_eq!(
            mgr.delete_tombstones.lock().unwrap().get("rwfail"),
            Some(&5),
            "tombstone 在旧 save 拒绝后仍须存在"
        );
    }
    // ── S-02b: store reservation capability(§3.1/§3.2)──
    //
    // 以下测试引用尚未实现的 reservation 公开 API,故为 compile RED:
    //   RecoveryManager::reserve_task_namespace() -> Result<TaskNamespaceReservation, RecoveryError>
    //   RecoveryError::ReservationActive
    //   RecoveryManager::{load,save,update,remove,restore}_reserved(&reservation, ...)
    // Coder 实现这些 API 后转为 GREEN。

    /// §3.1:reserve_task_namespace 扫描全部 task_ key 并用同一 header classifier 分类;
    /// 遇 future/invalid 即返回 typed error,不得创建 reservation。
    #[test]
    fn s02b_reserve_fails_on_future_or_invalid_without_creating_reservation() {
        // (a) future schema → Unsupported,无 reservation
        {
            let tmp = tempfile::tempdir().unwrap();
            let store = KvStore::open(tmp.path()).unwrap();
            let mgr = RecoveryManager::new(store);
            let found_version = SNAPSHOT_SCHEMA_VERSION + 1;
            let raw = future_legacy_record_raw("future", found_version);
            write_raw_task(tmp.path(), "future", &raw);

            let err = mgr.reserve_task_namespace().unwrap_err();
            assert_unsupported(err, "task_future", found_version);

            // 不创建 reservation:普通 batch API 不得返回 ReservationActive。
            let result = mgr.load_all_task_snapshots().unwrap();
            assert!(
                result
                    .unsupported_schema
                    .iter()
                    .any(|p| p.key == "task_future")
            );
        }
        // (b) invalid schema → InvalidData,无 reservation
        {
            let tmp = tempfile::tempdir().unwrap();
            let store = KvStore::open(tmp.path()).unwrap();
            let mgr = RecoveryManager::new(store);
            let raw = r#"{"schemaVersion":"8","task_id":"bad","url":"u","save_path":"p","file_size":1,"downloaded":0,"completed_fragments":[],"total_fragments":1,"status":"paused"}"#;
            write_raw_task(tmp.path(), "bad", raw);

            let err = mgr.reserve_task_namespace().unwrap_err();
            assert_invalid_data(err, "task_bad");

            // 不创建 reservation:普通单 key load 不得返回 ReservationActive。
            assert_invalid_data(mgr.load_task_snapshot("bad").unwrap_err(), "task_bad");
        }
    }

    /// §3.2:活跃 reservation 期间,普通 load/save/update/remove/restore/batch API
    /// 一律返回 `ReservationActive`,不得先 scan 后照常执行或绕过。
    #[test]
    fn s02b_active_reservation_blocks_normal_apis_with_reservation_active() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let snap = make_snapshot("active", tachyon_core::DownloadState::Downloading);
        mgr.save_task_snapshot(&snap).unwrap();

        let reservation = mgr.reserve_task_namespace().unwrap();

        fn expect_reservation_active(err: RecoveryError) {
            match err {
                RecoveryError::ReservationActive => {}
                other => panic!("expected ReservationActive, got {other:?}"),
            }
        }

        expect_reservation_active(mgr.load_task_snapshot("active").unwrap_err());
        expect_reservation_active(mgr.save_task_snapshot(&snap).unwrap_err());
        expect_reservation_active(mgr.update_snapshot("active", |_| {}).unwrap_err());
        expect_reservation_active(mgr.remove_task("active").unwrap_err());
        expect_reservation_active(mgr.restore_task_snapshot(&snap).unwrap_err());
        // batch read API 同样被 reservation 拦截
        expect_reservation_active(mgr.load_all_task_snapshots().unwrap_err());
        expect_reservation_active(mgr.recover_pending_snapshots().unwrap_err());
    }

    /// §3.1:reserved 变体在活跃 reservation 下可正常执行,验证 manager identity + nonce。
    #[test]
    fn s02b_reserved_variants_succeed_under_active_reservation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let snap = make_snapshot("res", tachyon_core::DownloadState::Downloading);
        mgr.save_task_snapshot(&snap).unwrap();

        let reservation = mgr.reserve_task_namespace().unwrap();

        // reserved load
        let loaded = mgr.load_reserved(&reservation, "res").unwrap().unwrap();
        assert_eq!(loaded.id, "res");

        // reserved save(bump revision)
        let mut next = snap.clone();
        next.downloaded = 999;
        mgr.save_reserved(&reservation, &next).unwrap();
        let after = mgr.load_reserved(&reservation, "res").unwrap().unwrap();
        assert_eq!(after.downloaded, 999);
        assert!(after.revision > loaded.revision);

        // reserved update
        mgr.update_reserved(&reservation, "res", |s| s.downloaded = 1234)
            .unwrap();
        let after2 = mgr.load_reserved(&reservation, "res").unwrap().unwrap();
        assert_eq!(after2.downloaded, 1234);

        // reserved remove + restore(tombstone 经 reserved 路径流转)
        assert!(mgr.remove_reserved(&reservation, "res").unwrap());
        mgr.restore_reserved(&reservation, &snap).unwrap();
        let restored = mgr.load_reserved(&reservation, "res").unwrap().unwrap();
        assert_eq!(restored.id, "res");
    }

    /// §3.1 Drop:reservation drop 仅释放匹配的 active capability;
    /// 释放后普通 API 恢复可用,且下次 reserve 成功。
    #[test]
    fn s02b_dropping_reservation_releases_it_and_next_reserve_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let store = KvStore::open(tmp.path()).unwrap();
        let mgr = RecoveryManager::new(store);
        let snap = make_snapshot("drop", tachyon_core::DownloadState::Downloading);
        mgr.save_task_snapshot(&snap).unwrap();

        {
            let _reservation = mgr.reserve_task_namespace().unwrap();
            // active:普通 API 被拒
            match mgr.load_task_snapshot("drop").unwrap_err() {
                RecoveryError::ReservationActive => {}
                other => panic!("expected ReservationActive, got {other:?}"),
            }
        }
        // dropped:普通 API 恢复可用
        let loaded = mgr.load_task_snapshot("drop").unwrap().unwrap();
        assert_eq!(loaded.id, "drop");
        // 下次 reserve 成功
        let _r2 = mgr.reserve_task_namespace().unwrap();
    }
}
