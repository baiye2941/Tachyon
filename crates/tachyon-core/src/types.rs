//! 核心标识类型

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DownloadError;

/// 任务唯一标识
pub type TaskId = Uuid;

/// 下载任务状态
///
/// A-02: 使用 `strum::Display` 和 `strum::EnumString` 自动派生，
/// Display / FromStr / serde 三者通过 `#[strum(serialize_all = "lowercase")]`
/// 和 `#[serde(rename_all = "lowercase")]` 保持一致，新增变体只需添加枚举项。
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    Default,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum DownloadState {
    #[default]
    Pending,
    Connecting,
    Downloading,
    Paused,
    Resuming,
    Verifying,
    Completed,
    Failed,
    Cancelled,
}

impl DownloadState {
    pub fn try_transition(&self, next: DownloadState) -> Result<DownloadState, DownloadError> {
        use DownloadState::*;
        let valid = matches!(
            (self, &next),
            (Pending, Connecting)
                | (Connecting, Downloading)
                | (Connecting, Failed)
                | (Connecting, Cancelled)
                | (Downloading, Paused)
                | (Downloading, Verifying)
                | (Downloading, Failed)
                | (Downloading, Cancelled)
                | (Paused, Resuming)
                | (Paused, Failed)
                | (Paused, Cancelled)
                | (Resuming, Downloading)
                | (Resuming, Failed)
                | (Resuming, Cancelled)
                | (Verifying, Completed)
                | (Verifying, Failed)
                | (Verifying, Cancelled)
                | (Failed, Pending)
                | (Cancelled, Pending)
        );
        if valid {
            Ok(next)
        } else {
            Err(DownloadError::Config(format!(
                "非法状态转换: {self:?} -> {next:?}"
            )))
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            DownloadState::Completed | DownloadState::Failed | DownloadState::Cancelled
        )
    }
}

/// 任务控制命令
///
/// 从前端/用户发出的控制命令,在 engine 层翻译为 `DownloadState`。
/// 控制通道使用此类型实现命令与状态分离,并避免 app 层为每个下载 spawn 翻译任务。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskCommand {
    /// 启动下载
    Start,
    /// 暂停下载
    Pause,
    /// 恢复下载
    Resume,
    /// 取消下载
    Cancel,
}

impl TaskCommand {
    /// 将控制命令翻译为引擎内部状态
    pub fn to_download_state(self) -> DownloadState {
        match self {
            Self::Start => DownloadState::Downloading,
            Self::Pause => DownloadState::Paused,
            Self::Resume => DownloadState::Downloading,
            Self::Cancel => DownloadState::Cancelled,
        }
    }
}

/// 暂停状态信息，用于跟踪暂停超时
///
/// CLAUDE.md 要求: paused 状态 MUST 有时间上限，不能永久暂停
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PauseInfo {
    /// 暂停开始时间(UNIX 时间戳，秒)
    pub paused_at_secs: u64,
    /// 最大暂停持续时间(秒)
    pub max_duration_secs: u64,
}

/// 获取当前 UNIX 时间戳(秒)。
///
/// Miri 隔离模式下 `clock_gettime` 不可用，返回确定性默认值以确保测试可运行。
#[cfg(not(miri))]
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(miri)]
fn now_unix_secs() -> u64 {
    // Miri 隔离模式: 返回一个固定的"当前"时间戳 (2024-01-01T00:00:00 UTC)
    1_704_067_200
}

impl PauseInfo {
    /// 创建新的暂停信息
    pub fn new(max_duration_secs: u64) -> Self {
        Self {
            paused_at_secs: now_unix_secs(),
            max_duration_secs,
        }
    }

    /// 暂停是否已超时
    pub fn is_expired(&self) -> bool {
        let now = now_unix_secs();
        now.saturating_sub(self.paused_at_secs) >= self.max_duration_secs
    }

    /// 剩余暂停时间(秒)，超时返回 0
    pub fn remaining_secs(&self) -> u64 {
        let now = now_unix_secs();
        let elapsed = now.saturating_sub(self.paused_at_secs);
        self.max_duration_secs.saturating_sub(elapsed)
    }
}

/// 文件元数据
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileMetadata {
    /// 文件名
    pub file_name: String,
    /// 文件大小(字节),None 表示服务端未返回 Content-Length
    pub file_size: Option<u64>,
    /// MIME 类型
    pub content_type: Option<String>,
    /// 支持分片下载
    pub supports_range: bool,
    /// ETag
    pub etag: Option<String>,
    /// 最后修改时间
    pub last_modified: Option<String>,
    /// 多文件布局(BitTorrent 多文件 torrent 用)
    ///
    /// None 表示单文件(HTTP/FTP/单文件 torrent),init_storage 走单文件路径。
    /// Some 表示多文件,file_layout 描述各文件的 (file_id, offset, len, name),
    /// init_storage 据此构造 StorageSet::Multi,download_range_stream 据此拆分跨文件 range。
    #[serde(default)]
    pub file_layout: Option<FileLayout>,
    /// 协议是否直接管理存储(P2-4: BT 自定义 Storage)
    ///
    /// true 时表示协议层(BT custom Storage)直接写入目标文件,
    /// 引擎跳过 write_all_at(消除双存储写放大),仅消费流追踪进度+完成信号。
    /// false 时引擎正常写入(HTTP/FTP/未启自定义 Storage 的 BT)。
    #[serde(default)]
    pub protocol_managed_storage: bool,
    /// 审计 HTTP-13:探测/重定向后的最终 host(用于 per-host 许可记账)
    ///
    /// None 表示未解析或非 HTTP;引擎 `request_host` 优先使用该值,避免 origin 与 CDN 记账分裂。
    #[serde(default)]
    pub resolved_host: Option<String>,
}

/// HTTP 对象身份(ETag / Last-Modified / size)
///
/// 用于 resume 准入、Range `If-Range` 与镜像兼容筛选,防止同长度不同版本静默拼接。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ObjectIdentity {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub file_size: Option<u64>,
}

impl ObjectIdentity {
    /// 从探测元数据提取对象身份。
    pub fn from_metadata(meta: &FileMetadata) -> Self {
        Self {
            etag: meta.etag.clone(),
            last_modified: meta.last_modified.clone(),
            file_size: meta.file_size,
        }
    }

    /// 是否为 strong ETag(非 `W/` / `w/` 前缀)。
    pub fn is_strong_etag(etag: &str) -> bool {
        let t = etag.trim();
        !(t.starts_with("W/") || t.starts_with("w/"))
    }

    /// 规范化 ETag 比较键:去空白、可选弱标记、两侧引号。
    pub fn normalize_etag(etag: &str) -> String {
        let mut t = etag.trim();
        if let Some(rest) = t.strip_prefix("W/").or_else(|| t.strip_prefix("w/")) {
            t = rest.trim();
        }
        if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
            t[1..t.len() - 1].to_string()
        } else {
            t.to_string()
        }
    }

    /// 适合写入 `If-Range` 的值:仅 strong ETag 或 Last-Modified。
    ///
    /// weak ETag 不得作为 `If-Range`(RFC 9110)。
    pub fn if_range_value(&self) -> Option<String> {
        if let Some(ref etag) = self.etag
            && Self::is_strong_etag(etag)
        {
            return Some(etag.trim().to_string());
        }
        self.last_modified
            .as_ref()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// resume 是否可安全沿用已完成/部分分片。
    ///
    /// - 双方 strong ETag 规范化后相等 → 可
    /// - 无 strong 时双方 Last-Modified 相等 → 可
    /// - 仅 size 或未知 → 不可(防同长换版)
    pub fn compatible_for_resume(&self, remote: &Self) -> bool {
        if let (Some(a), Some(b)) = (self.strong_etag_key(), remote.strong_etag_key()) {
            return a == b;
        }
        // 任一侧仅有 weak/无 ETag 时不靠 ETag 证明
        if let (Some(a), Some(b)) = (
            self.last_modified.as_ref().map(|s| s.trim()),
            remote.last_modified.as_ref().map(|s| s.trim()),
        ) && !a.is_empty()
            && !b.is_empty()
        {
            return a == b;
        }
        false
    }

    /// 镜像源是否可与 baseline 混拼同一逻辑对象。
    ///
    /// 规则与 resume 相同:仅 strong ETag 或 Last-Modified 可证明;仅 size 不可混拼。
    pub fn compatible_for_mirror(&self, other: &Self) -> bool {
        self.compatible_for_resume(other)
    }

    fn strong_etag_key(&self) -> Option<String> {
        self.etag.as_ref().and_then(|e| {
            if Self::is_strong_etag(e) {
                Some(Self::normalize_etag(e))
            } else {
                None
            }
        })
    }
}

/// 多文件布局:全局偏移 ↔ (file_id, 文件内偏移) 的双向折算
///
/// 用于 BitTorrent 多文件 torrent:引擎按 torrent 全局字节流切分片
/// (`plan_fragments` 按总长切),而 `FileStream`/存储后端都绑定单个 file_id。
/// `FileLayout` 把全局 `[start, end]` 拆成各文件内的段,供读取侧
/// (`download_range_stream`)和写入侧(`storage.write_at`)共享同一映射。
///
/// 单文件 torrent 退化为单元素列表,行为等价于现有单文件路径。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileLayout {
    /// 按 global_offset 升序排列的文件段
    files: Vec<FileSpan>,
}

/// `FileLayout::try_from_spans` 的校验错误类型(F-09)
///
/// 区分五种不变量违反,便于调用方精确报告布局损坏原因:
/// - `FileIdGap`:file_id 缺号(如 0,2 缺 1)
/// - `FileIdDuplicate`:file_id 重复(如 0,0)
/// - `OffsetGap`:全局 offset 出现空洞(段间未紧接)
/// - `OffsetOverlap`:全局 offset 重叠(段间相互覆盖)
/// - `LenOverflow`:`global_offset + len` 溢出 u64
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    FileIdGap,
    FileIdDuplicate,
    OffsetGap,
    OffsetOverlap,
    LenOverflow,
}

/// FileLayout 中的一个文件段
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSpan {
    /// 文件在 torrent 内的索引(librqbit file_infos 下标)
    pub file_id: usize,
    /// 文件在 torrent 全局字节流的起点
    pub global_offset: u64,
    /// 文件长度
    pub len: u64,
    /// 文件相对名(用于落盘路径)
    pub name: String,
}

impl FileLayout {
    /// 单文件快捷构造
    pub fn single(name: String, len: u64) -> Self {
        Self {
            files: vec![FileSpan {
                file_id: 0,
                global_offset: 0,
                len,
                name,
            }],
        }
    }

    /// 从文件段列表构造(会按 global_offset 排序,确保升序不变量)
    ///
    /// 保留旧行为(仅排序,不校验)以兼容需要绕过校验构造畸形 layout 的场景
    /// (如 debug_assert 测试)。生产代码 SHOULD 用 `try_from_spans` 获取
    /// 结构化错误,本方法仅用于调用方已确保 spans 合法的快速路径。
    pub fn from_spans(mut spans: Vec<FileSpan>) -> Self {
        spans.sort_by_key(|s| s.global_offset);
        Self { files: spans }
    }

    /// F-09:从文件段列表构造并做边界校验
    ///
    /// 校验规则(排序后逐段比对):
    /// 1. `file_id` 从 0 递增,缺号 → `FileIdGap`,回退/重复 → `FileIdDuplicate`
    /// 2. `global_offset` 严格紧接上一段末尾(首段可从任意 offset 起),
    ///    空洞 → `OffsetGap`,重叠 → `OffsetOverlap`
    /// 3. `global_offset + len` 不得溢出,否则 `LenOverflow`
    ///
    /// 空列表通过校验(退化为零长度布局,`total_len == 0`)。
    pub fn try_from_spans(mut spans: Vec<FileSpan>) -> Result<Self, LayoutError> {
        spans.sort_by_key(|s| s.global_offset);
        // expected_offset = None 表示首段,首段可从任意 global_offset 起(不强制 0),
        // 后续段必须紧接上一段末尾。这允许单文件布局起始 offset 非 0(合法),
        // 同时拒绝多文件间的 offset 空洞/重叠。
        let mut expected_offset: Option<u64> = None;
        for (expected_file_id, span) in spans.iter().enumerate() {
            if span.file_id != expected_file_id {
                if span.file_id < expected_file_id {
                    return Err(LayoutError::FileIdDuplicate);
                }
                return Err(LayoutError::FileIdGap);
            }
            if let Some(expected) = expected_offset
                && span.global_offset != expected
            {
                if span.global_offset > expected {
                    return Err(LayoutError::OffsetGap);
                }
                return Err(LayoutError::OffsetOverlap);
            }
            let end = span
                .global_offset
                .checked_add(span.len)
                .ok_or(LayoutError::LenOverflow)?;
            expected_offset = Some(end);
        }
        Ok(Self { files: spans })
    }

    /// 全局总长
    pub fn total_len(&self) -> u64 {
        self.files
            .last()
            // 修复 MEDIUM-1:global_offset + len 可能溢出(恶意 torrent 元数据构造超大 offset),
            // 用 saturating_add 饱和到 u64::MAX,与 split_range 的 span_end 一致
            .map(|f| f.global_offset.saturating_add(f.len))
            .unwrap_or(0)
    }

    /// 文件段数量
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// 各文件的相对名列表(按 file_id 升序)
    ///
    /// 供 `validate_multi_save_paths` 取各文件 relative_filename 做落盘路径校验。
    pub fn file_names(&self) -> Vec<String> {
        self.files.iter().map(|f| f.name.clone()).collect()
    }

    /// 按排序后的完整文件段列表(只读视图)
    ///
    /// 供调用方(如 `StorageSet::multi`)在构造时复用已校验的 spans,
    /// 或将已校验 layout 转回 `Vec<FileSpan>` 重新走 `try_from_spans`。
    pub fn file_spans(&self) -> &[FileSpan] {
        &self.files
    }

    /// 把全局闭区间 `[start, end]` 拆成各文件内的段
    ///
    /// 返回 `Vec<(file_id, file_local_start, file_local_end)>`,按文件顺序排列,
    /// 各段的 file_local 坐标是该文件内偏移(从 0 开始)。
    /// 跨文件边界的 range 会被拆成多段;完全在单文件内的返回单段。
    /// start > end 返回空 Vec(非法 range)。
    pub fn split_range(&self, start: u64, end: u64) -> Vec<(usize, u64, u64)> {
        if start > end {
            return Vec::new();
        }
        // 修复 BUG-E:end+1 在 end=u64::MAX 时溢出。用 saturating_add,饱和到 MAX
        // (语义:exclusive 上界,饱和到 MAX 等价于"到末尾")
        let end_exclusive = end.saturating_add(1);
        let mut out = Vec::new();
        for span in &self.files {
            let span_start = span.global_offset;
            // 修复 MEDIUM-1:global_offset + len 可能溢出(恶意 torrent 元数据),
            // 用 saturating_add 饱和到 u64::MAX(语义:到末尾)
            let span_end = span.global_offset.saturating_add(span.len); // exclusive
            // 该文件区间 [span_start, span_end) 与 [start, end_exclusive) 的交集
            let lo = start.max(span_start);
            let hi = end_exclusive.min(span_end);
            if lo < hi {
                // 有交集,折算到文件内偏移
                let local_start = lo - span_start;
                let local_end = hi - 1 - span_start; // 闭区间末字节
                out.push((span.file_id, local_start, local_end));
            }
            // 文件已在 end 之前结束,后续无需再查
            if span_end > end_exclusive {
                break;
            }
        }
        out
    }
}

/// 分片信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentInfo {
    /// 分片索引
    pub index: u32,
    /// 起始字节偏移
    pub start: u64,
    /// 结束字节偏移(含)
    pub end: u64,
    /// 分片大小(字节)
    pub size: u64,
    /// 下载进度(已下载字节数)
    pub downloaded: u64,
    /// 分片校验哈希
    pub hash: Option<String>,
}

impl FragmentInfo {
    pub fn new(index: u32, start: u64, end: u64, size: u64) -> Result<Self, DownloadError> {
        // 使用 checked_add 防止 u64 溢出,并以错误传播代替 panic。
        // end/start/size 可能来自服务器响应(如 Content-Range),为服务器可控值,
        // 不应在服务器返回极端值(如 end=u64::MAX)时 panic 导致整个进程崩溃。
        let end_plus_1 = end
            .checked_add(1)
            .ok_or_else(|| DownloadError::Fragment("FragmentInfo: end + 1 溢出".into()))?;
        let start_plus_size = start
            .checked_add(size)
            .ok_or_else(|| DownloadError::Fragment("FragmentInfo: start + size 溢出".into()))?;
        if end_plus_1 != start_plus_size {
            return Err(DownloadError::Fragment(format!(
                "FragmentInfo invariant 违反: end + 1 == start + size 不成立, got end={end}, start={start}, size={size}"
            )));
        }
        Ok(Self {
            index,
            start,
            end,
            size,
            downloaded: 0,
            hash: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskProgress {
    pub downloaded: u64,
    pub speed: u64,
    /// 进度百分比(0.0 ~ 1.0)
    pub progress: f64,
    pub fragments_done: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadStateChange {
    pub task_id: String,
    pub new_state: DownloadState,
}

/// 分片进度事件
///
/// 通过 `progress_tx` 通道发送给上层(tachyon-app)。
/// 四变体:控制帧(PlanComplete,一次性可靠)、开始帧(Started,低频可丢)、
/// 数据帧(Chunk,高频可丢)、重试帧(Retry,低频可丢)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FragmentProgress {
    /// plan 完成:携带真实分片总数 + 续传已完成索引 + 初始并发度
    ///
    /// 仅在 `DownloadTask::plan()` 末尾发送一次。
    /// 用 `send().await`——此时 channel 必为空(plan 是第一个事件),不会阻塞。
    PlanComplete {
        /// 真实分片总数(来自 plan_fragments,非 probe 估算)
        total: u32,
        /// 续传恢复的已完成分片索引(state==Done 的 index 列表)
        completed_indices: Vec<u32>,
        /// 初始并发度(调度器 recommendation.concurrency)
        initial_concurrency: u32,
    },
    /// 分片下载进度(原 struct 三字段,语义不变)
    ///
    /// 增量用 `try_send`(可丢),完成用 `send().await`
    Chunk {
        fragment_index: u32,
        completed: bool,
        fragment_downloaded: u64,
    },
    /// worker 实际开始下载分片(`download_single_fragment` 入口,获取信号量后)
    ///
    /// 用 `try_send`(可丢):丢失仅导致该分片短暂不在 downloading_set,
    /// 不影响正确性。重试时重复发送,app 层 `mark_downloading` 幂等吸收。
    Started { fragment_index: u32 },
    /// 分片可重试失败:即将退避后再次 attempt
    ///
    /// 在 spawn 重试循环判定 `will retry` 后 `try_send`(可丢)。
    /// `attempt` 为该分片本次将进行的 attempt 序号(从 1 起,对应失败后递增的计数)。
    /// app 层每收到一次累加 `TaskInfo.retry_count`(任务级累计重试次数)。
    Retry {
        fragment_index: u32,
        attempt: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pause_info_creation() {
        let info = PauseInfo::new(300);
        assert_eq!(info.max_duration_secs, 300);
        assert!(!info.is_expired(), "新创建的暂停信息不应过期");
        assert!(info.remaining_secs() <= 300);
        assert!(info.remaining_secs() > 0);
    }

    #[test]
    fn test_pause_info_expired() {
        let info = PauseInfo {
            paused_at_secs: 0, // UNIX 纪元
            max_duration_secs: 1,
        };
        assert!(info.is_expired(), "很久以前的暂停应已过期");
        assert_eq!(info.remaining_secs(), 0);
    }

    #[test]
    fn test_pause_info_serialization() {
        let info = PauseInfo::new(600);
        let json = serde_json::to_string(&info).unwrap();
        let deserialized: PauseInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.max_duration_secs, 600);
    }

    #[test]
    fn test_download_state_variants() {
        assert_ne!(DownloadState::Pending, DownloadState::Downloading);
        assert_ne!(DownloadState::Completed, DownloadState::Failed);
        assert_eq!(DownloadState::Paused, DownloadState::Paused);
    }

    #[test]
    fn test_download_state_clone() {
        let state = DownloadState::Downloading;
        let cloned = state;
        assert_eq!(state, cloned);
    }

    #[test]
    fn test_file_metadata_with_size() {
        let meta = FileMetadata {
            file_name: "test.bin".into(),
            file_size: Some(1024),
            content_type: Some("application/octet-stream".into()),
            supports_range: true,
            etag: Some("\"abc\"".into()),
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        assert_eq!(meta.file_size, Some(1024));
        assert!(meta.supports_range);
    }

    #[test]
    fn test_file_metadata_unknown_size() {
        let meta = FileMetadata {
            file_name: "stream.mp4".into(),
            file_size: None,
            content_type: None,
            supports_range: false,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        assert!(meta.file_size.is_none());
        assert!(!meta.supports_range);
    }

    #[test]
    fn test_fragment_info() {
        let frag = FragmentInfo {
            index: 0,
            start: 0,
            end: 999,
            size: 1000,
            downloaded: 500,
            hash: None,
        };
        assert_eq!(frag.index, 0);
        assert_eq!(frag.size, 1000);
        assert_eq!(frag.downloaded, 500);
    }

    #[test]
    fn test_task_id_generation() {
        let id1 = TaskId::new_v4();
        let id2 = TaskId::new_v4();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_file_metadata_serialization() {
        let meta = FileMetadata {
            file_name: "test.bin".into(),
            file_size: Some(1024),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let deserialized: FileMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.file_name, "test.bin");
        assert_eq!(deserialized.file_size, Some(1024));
    }

    #[test]
    fn test_object_identity_strong_etag_compatible() {
        let a = ObjectIdentity {
            etag: Some("\"abc\"".into()),
            last_modified: None,
            file_size: Some(100),
        };
        let b = ObjectIdentity {
            etag: Some("\"abc\"".into()),
            last_modified: None,
            file_size: Some(100),
        };
        assert!(a.compatible_for_resume(&b));
        assert!(a.compatible_for_mirror(&b));
        assert_eq!(a.if_range_value().as_deref(), Some("\"abc\""));
    }

    #[test]
    fn test_object_identity_strong_etag_mismatch_rejects() {
        let old = ObjectIdentity {
            etag: Some("\"v1\"".into()),
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            file_size: Some(100),
        };
        let new = ObjectIdentity {
            etag: Some("\"v2\"".into()),
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            file_size: Some(100),
        };
        assert!(!old.compatible_for_resume(&new));
        assert!(!old.compatible_for_mirror(&new));
    }

    #[test]
    fn test_object_identity_weak_etag_not_if_range_and_not_safe_alone() {
        let a = ObjectIdentity {
            etag: Some("W/\"weak\"".into()),
            last_modified: None,
            file_size: Some(100),
        };
        let b = ObjectIdentity {
            etag: Some("W/\"weak\"".into()),
            last_modified: None,
            file_size: Some(100),
        };
        assert!(!ObjectIdentity::is_strong_etag("W/\"weak\""));
        assert!(a.if_range_value().is_none());
        // 仅 weak ETag + size 不得证明 resume/mirror 兼容
        assert!(!a.compatible_for_resume(&b));
        assert!(!a.compatible_for_mirror(&b));
    }

    #[test]
    fn test_object_identity_last_modified_fallback() {
        let a = ObjectIdentity {
            etag: None,
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            file_size: Some(50),
        };
        let same = ObjectIdentity {
            etag: None,
            last_modified: Some("Mon, 01 Jan 2024 00:00:00 GMT".into()),
            file_size: Some(50),
        };
        let different = ObjectIdentity {
            etag: None,
            last_modified: Some("Tue, 02 Jan 2024 00:00:00 GMT".into()),
            file_size: Some(50),
        };
        assert!(a.compatible_for_resume(&same));
        assert!(!a.compatible_for_resume(&different));
        assert_eq!(
            a.if_range_value().as_deref(),
            Some("Mon, 01 Jan 2024 00:00:00 GMT")
        );
    }

    #[test]
    fn test_object_identity_size_only_not_safe() {
        let a = ObjectIdentity {
            etag: None,
            last_modified: None,
            file_size: Some(1024),
        };
        let b = ObjectIdentity {
            etag: None,
            last_modified: None,
            file_size: Some(1024),
        };
        assert!(!a.compatible_for_resume(&b));
        assert!(!a.compatible_for_mirror(&b));
        assert!(a.if_range_value().is_none());
    }

    #[test]
    fn test_object_identity_from_metadata() {
        let meta = FileMetadata {
            file_name: "x.bin".into(),
            file_size: Some(9),
            content_type: None,
            supports_range: true,
            etag: Some("\"e\"".into()),
            last_modified: Some("Wed, 01 Jan 2025 00:00:00 GMT".into()),
            file_layout: None,
            protocol_managed_storage: false,
            resolved_host: None,
        };
        let id = ObjectIdentity::from_metadata(&meta);
        assert_eq!(id.etag.as_deref(), Some("\"e\""));
        assert_eq!(id.file_size, Some(9));
    }

    #[test]
    fn test_download_state_serialization() {
        let state = DownloadState::Downloading;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"downloading\"");
        let deserialized: DownloadState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, DownloadState::Downloading);
    }

    #[test]
    fn test_try_transition_valid_paths() {
        use DownloadState::*;
        let valid = [
            (Pending, Connecting),
            (Connecting, Downloading),
            (Connecting, Failed),
            (Connecting, Cancelled),
            (Downloading, Paused),
            (Downloading, Verifying),
            (Downloading, Failed),
            (Downloading, Cancelled),
            (Paused, Resuming),
            (Paused, Failed),
            (Paused, Cancelled),
            (Resuming, Downloading),
            (Resuming, Failed),
            (Resuming, Cancelled),
            (Verifying, Completed),
            (Verifying, Failed),
            (Verifying, Cancelled),
            (Failed, Pending),
            (Cancelled, Pending),
        ];
        for (from, to) in valid {
            assert!(
                from.try_transition(to).is_ok(),
                "合法转换应成功: {from:?} -> {to:?}"
            );
        }
    }

    #[test]
    fn test_try_transition_invalid_paths() {
        use DownloadState::*;
        let invalid = [
            (Pending, Completed),
            (Pending, Downloading),
            (Completed, Pending),
            (Completed, Failed),
            (Downloading, Pending),
            (Failed, Downloading),
            (Paused, Downloading),
        ];
        for (from, to) in invalid {
            assert!(
                from.try_transition(to).is_err(),
                "非法转换应被拒绝: {from:?} -> {to:?}"
            );
        }
    }

    #[test]
    fn test_is_terminal() {
        use DownloadState::*;
        assert!(!Pending.is_terminal());
        assert!(!Connecting.is_terminal());
        assert!(!Downloading.is_terminal());
        assert!(!Paused.is_terminal());
        assert!(!Resuming.is_terminal());
        assert!(!Verifying.is_terminal());
        assert!(Completed.is_terminal());
        assert!(Failed.is_terminal());
        assert!(Cancelled.is_terminal());
    }

    // -----------------------------------------------------------------------
    // P1: TaskCommand / FragmentInfo / PauseInfo 边界测试
    // -----------------------------------------------------------------------

    #[test]
    fn test_task_command_to_download_state_mappings() {
        assert_eq!(
            TaskCommand::Start.to_download_state(),
            DownloadState::Downloading
        );
        assert_eq!(
            TaskCommand::Pause.to_download_state(),
            DownloadState::Paused
        );
        assert_eq!(
            TaskCommand::Resume.to_download_state(),
            DownloadState::Downloading
        );
        assert_eq!(
            TaskCommand::Cancel.to_download_state(),
            DownloadState::Cancelled
        );
    }

    #[test]
    fn test_fragment_info_new_normal() {
        let frag = FragmentInfo::new(2, 10, 19, 10).expect("合法分片应构造成功");
        assert_eq!(frag.index, 2);
        assert_eq!(frag.start, 10);
        assert_eq!(frag.end, 19);
        assert_eq!(frag.size, 10);
        assert_eq!(frag.downloaded, 0);
        assert!(frag.hash.is_none());
    }

    #[test]
    fn test_fragment_info_new_end_overflow_returns_err() {
        // F-10:服务器可控的 end=u64::MAX 不应 panic,而应返回错误。
        let result = FragmentInfo::new(0, 0, u64::MAX, 0);
        assert!(result.is_err(), "end + 1 溢出应返回错误而非 panic");
        assert!(result.unwrap_err().to_string().contains("end + 1 溢出"));
    }

    #[test]
    fn test_fragment_info_new_start_size_overflow_returns_err() {
        // F-10:start + size 溢出应返回错误而非 panic。
        let result = FragmentInfo::new(0, u64::MAX - 1, 0, 3);
        assert!(result.is_err(), "start + size 溢出应返回错误而非 panic");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("start + size 溢出")
        );
    }

    #[test]
    fn test_pause_info_remaining_secs_boundaries() {
        // max_duration 为 0 时立即无剩余
        let info = PauseInfo {
            paused_at_secs: 0,
            max_duration_secs: 0,
        };
        assert_eq!(info.remaining_secs(), 0);

        // 已过期时剩余为 0
        let expired = PauseInfo {
            paused_at_secs: 0,
            max_duration_secs: 1,
        };
        assert_eq!(expired.remaining_secs(), 0);

        // 未过期时剩余不超过 max_duration_secs 且大于 0
        let now = now_unix_secs();
        let active = PauseInfo {
            paused_at_secs: now,
            max_duration_secs: 60,
        };
        let remaining = active.remaining_secs();
        assert!(remaining > 0 && remaining <= 60, "remaining={remaining}");
    }

    // ===== FileLayout 折算测试 =====

    #[test]
    fn test_file_layout_single_file_degenerates_to_one_span() {
        let layout = FileLayout::single("data.bin".into(), 8192);
        assert_eq!(layout.file_count(), 1);
        assert_eq!(layout.total_len(), 8192);

        // 全文件 [0, 8191] → 单段 (file_id=0, local 0..8191)
        let segs = layout.split_range(0, 8191);
        assert_eq!(segs, vec![(0, 0, 8191)]);
    }

    #[test]
    fn test_file_layout_split_range_within_single_file() {
        let layout = FileLayout::single("data.bin".into(), 8192);
        // 子区间完全在单文件内
        let segs = layout.split_range(1500, 3500);
        assert_eq!(segs, vec![(0, 1500, 3500)]);
    }

    #[test]
    fn test_file_layout_split_range_across_file_boundary() {
        // 两文件:file0 [0, 4095], file1 [4096, 8191]
        let layout = FileLayout::from_spans(vec![
            FileSpan {
                file_id: 0,
                global_offset: 0,
                len: 4096,
                name: "a.bin".into(),
            },
            FileSpan {
                file_id: 1,
                global_offset: 4096,
                len: 4096,
                name: "b.bin".into(),
            },
        ]);
        assert_eq!(layout.total_len(), 8192);

        // 跨边界 [3000, 5000] → file0 [3000,4095] + file1 [0,904]
        // file1 全局 [4096, 5000],文件内偏移 5000-4096=904
        let segs = layout.split_range(3000, 5000);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0], (0, 3000, 4095), "file0 段");
        assert_eq!(segs[1], (1, 0, 904), "file1 段: 5000-4096=904");
    }

    #[test]
    fn test_file_layout_split_range_exactly_at_boundary() {
        let layout = FileLayout::from_spans(vec![
            FileSpan {
                file_id: 0,
                global_offset: 0,
                len: 4096,
                name: "a.bin".into(),
            },
            FileSpan {
                file_id: 1,
                global_offset: 4096,
                len: 4096,
                name: "b.bin".into(),
            },
        ]);
        // range 恰好落在 file0 末字节 4095
        let segs = layout.split_range(4095, 4095);
        assert_eq!(segs, vec![(0, 4095, 4095)]);

        // range 恰好落在 file1 首字节 4096
        let segs = layout.split_range(4096, 4096);
        assert_eq!(segs, vec![(1, 0, 0)]);
    }

    #[test]
    fn test_file_layout_split_range_across_three_files() {
        // 三文件各 1024
        let layout = FileLayout::from_spans(vec![
            FileSpan {
                file_id: 0,
                global_offset: 0,
                len: 1024,
                name: "a".into(),
            },
            FileSpan {
                file_id: 1,
                global_offset: 1024,
                len: 1024,
                name: "b".into(),
            },
            FileSpan {
                file_id: 2,
                global_offset: 2048,
                len: 1024,
                name: "c".into(),
            },
        ]);
        // 全局 [500, 2500] 跨三文件
        let segs = layout.split_range(500, 2500);
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0], (0, 500, 1023)); // a: [500, 1023]
        assert_eq!(segs[1], (1, 0, 1023)); // b: 全部
        assert_eq!(segs[2], (2, 0, 452)); // c: [0, 2500-2048=452]
    }

    #[test]
    fn test_file_layout_split_range_illegal_returns_empty() {
        let layout = FileLayout::single("x".into(), 100);
        assert!(
            layout.split_range(50, 49).is_empty(),
            "start > end 应返回空"
        );
    }

    /// 修复 MEDIUM-1:global_offset + len 溢出时不应 panic,应饱和到 u64::MAX
    #[test]
    fn test_file_layout_span_end_overflow_saturates() {
        // 构造一个 global_offset 接近 u64::MAX 的 span,len 使 global_offset+len 溢出
        let layout = FileLayout::from_spans(vec![FileSpan {
            file_id: 0,
            global_offset: u64::MAX - 10,
            len: 100,
            name: "overflow.bin".into(),
        }]);
        // total_len 应饱和到 u64::MAX,不 panic
        assert_eq!(layout.total_len(), u64::MAX);
        // split_range 应正常工作:span_end 饱和到 MAX,range [MAX-10, MAX-5] 命中该 span
        let segs = layout.split_range(u64::MAX - 10, u64::MAX - 5);
        assert_eq!(segs.len(), 1, "应命中溢出 span 的单段");
        assert_eq!(segs[0].0, 0, "file_id 应为 0");
        assert_eq!(segs[0].1, 0, "local_start 应为 0(span 起点即 range 起点)");
    }

    /// bench 缺口 1a:split_range 纯 CPU 折算开销 micro-bench
    ///
    /// split_range 是 magnet download_range_stream 和 storage Multi read/write 的公共
    /// 折算层,每次跨文件 range 调用一次。隔离测量其纯 CPU 开销(无 I/O),
    /// 确认在大文件多文件场景(16 文件,跨 15 边界)下不成为瓶颈。
    ///
    /// Miri 下跳过:Miri 是解释器,执行慢 ~1000x,性能断言(<10µs)在 Miri 下必然误判。
    #[test]
    #[cfg_attr(miri, ignore = "性能断言不适用于 Miri 解释器")]
    fn bench_split_range_cross_boundary() {
        // 16 个 1MB 文件,总 16MB
        let n_files = 16usize;
        let file_len = 1024 * 1024u64;
        let spans: Vec<FileSpan> = (0..n_files)
            .map(|i| FileSpan {
                file_id: i,
                global_offset: i as u64 * file_len,
                len: file_len,
                name: format!("f{i}"),
            })
            .collect();
        let layout = FileLayout::from_spans(spans);
        let total = layout.total_len();

        // 跨 15 个边界的 range:[0, total-1]
        let iterations = 100_000u32;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let segs = layout.split_range(0, total - 1);
            // 防优化:确认段数正确(16 段,每段一文件)
            debug_assert_eq!(segs.len(), n_files);
        }
        let elapsed = start.elapsed();
        let per_op_ns = elapsed.as_nanos() / iterations as u128;
        eprintln!(
            "split_range 跨 {n_files} 文件边界: {iterations} 次 {elapsed:?} = {per_op_ns} ns/op"
        );
        // 纯 CPU 折算应在微秒级以下;硬断言 <10µs(回归监控,防恶化)
        assert!(
            per_op_ns < 10_000,
            "split_range 单次应 <10µs,实际 {per_op_ns} ns"
        );
    }

    #[test]
    fn test_fragment_progress_plan_complete_serialization() {
        let progress = FragmentProgress::PlanComplete {
            total: 16,
            completed_indices: vec![0, 1, 2],
            initial_concurrency: 4,
        };
        let json = serde_json::to_string(&progress).unwrap();
        let de: FragmentProgress = serde_json::from_str(&json).unwrap();
        match de {
            FragmentProgress::PlanComplete {
                total,
                completed_indices,
                initial_concurrency,
            } => {
                assert_eq!(total, 16);
                assert_eq!(completed_indices, vec![0, 1, 2]);
                assert_eq!(initial_concurrency, 4);
            }
            FragmentProgress::Chunk { .. }
            | FragmentProgress::Started { .. }
            | FragmentProgress::Retry { .. } => {
                panic!("应为 PlanComplete")
            }
        }
    }

    #[test]
    fn test_fragment_progress_chunk_serialization() {
        let progress = FragmentProgress::Chunk {
            fragment_index: 5,
            completed: true,
            fragment_downloaded: 1024,
        };
        let json = serde_json::to_string(&progress).unwrap();
        let de: FragmentProgress = serde_json::from_str(&json).unwrap();
        match de {
            FragmentProgress::Chunk {
                fragment_index,
                completed,
                fragment_downloaded,
            } => {
                assert_eq!(fragment_index, 5);
                assert!(completed);
                assert_eq!(fragment_downloaded, 1024);
            }
            FragmentProgress::PlanComplete { .. }
            | FragmentProgress::Started { .. }
            | FragmentProgress::Retry { .. } => {
                panic!("应为 Chunk")
            }
        }
    }

    #[test]
    fn test_fragment_progress_started_serialization() {
        let progress = FragmentProgress::Started { fragment_index: 7 };
        let json = serde_json::to_string(&progress).unwrap();
        // camelCase rename_all 在 enum 上只对变体名生效,字段保持 snake_case
        // (与 PlanComplete/Chunk 变体约定一致)
        assert!(json.contains("\"started\""));
        assert!(json.contains("\"fragment_index\":7"));
        let de: FragmentProgress = serde_json::from_str(&json).unwrap();
        match de {
            FragmentProgress::Started { fragment_index } => {
                assert_eq!(fragment_index, 7);
            }
            FragmentProgress::PlanComplete { .. }
            | FragmentProgress::Chunk { .. }
            | FragmentProgress::Retry { .. } => {
                panic!("应为 Started")
            }
        }
    }

    /// 任务级 retry_count 聚合:Retry 变体可构造、匹配、serde 往返
    #[test]
    fn test_fragment_progress_retry_construct_and_serialization() {
        let progress = FragmentProgress::Retry {
            fragment_index: 3,
            attempt: 2,
        };
        match &progress {
            FragmentProgress::Retry {
                fragment_index,
                attempt,
            } => {
                assert_eq!(*fragment_index, 3);
                assert_eq!(*attempt, 2);
            }
            FragmentProgress::PlanComplete { .. }
            | FragmentProgress::Chunk { .. }
            | FragmentProgress::Started { .. } => {
                panic!("应为 Retry")
            }
        }
        let json = serde_json::to_string(&progress).unwrap();
        assert!(json.contains("\"retry\""));
        assert!(json.contains("\"fragment_index\":3"));
        assert!(json.contains("\"attempt\":2"));
        let de: FragmentProgress = serde_json::from_str(&json).unwrap();
        match de {
            FragmentProgress::Retry {
                fragment_index,
                attempt,
            } => {
                assert_eq!(fragment_index, 3);
                assert_eq!(attempt, 2);
            }
            FragmentProgress::PlanComplete { .. }
            | FragmentProgress::Chunk { .. }
            | FragmentProgress::Started { .. } => {
                panic!("反序列化应为 Retry")
            }
        }
    }

    #[test]
    fn test_file_layout_file_names() {
        // 覆盖 FileLayout::file_names()(L254-256)
        let layout = FileLayout {
            files: vec![
                FileSpan {
                    file_id: 0,
                    global_offset: 0,
                    len: 1024,
                    name: "part1.bin".into(),
                },
                FileSpan {
                    file_id: 1,
                    global_offset: 1024,
                    len: 2048,
                    name: "part2.bin".into(),
                },
            ],
        };
        let names = layout.file_names();
        assert_eq!(names, vec!["part1.bin", "part2.bin"]);
    }

    #[test]
    fn test_fragment_info_new_invariant_violation() {
        // 覆盖 end+1 != start+size 的 invariant 违反分支(L323-326)
        // index=0, start=0, end=100, size=50 → end+1=101 != start+size=50
        let result = FragmentInfo::new(0, 0, 100, 50);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invariant"));
    }

    // ===== F-09: FileLayout::try_from_spans 边界校验(RED) =====
    //
    // 审计 F-09 指出 from_spans(types.rs:336-339)仅排序,
    // 不验证 file_id 连续性/offset 空洞/重叠/溢出。下列测试要求新增:
    //   - enum LayoutError { FileIdGap, FileIdDuplicate, OffsetGap, OffsetOverlap, LenOverflow }
    //   - FileLayout::try_from_spans(spans: Vec<FileSpan>) -> Result<Self, LayoutError>

    /// 合法布局(双文件 file_id=0,1; offset=0,1000; len=1000,500)应 Ok
    #[test]
    fn test_try_from_spans_accepts_valid_layout() {
        let spans = vec![
            FileSpan {
                file_id: 0,
                global_offset: 0,
                len: 1000,
                name: "a".into(),
            },
            FileSpan {
                file_id: 1,
                global_offset: 1000,
                len: 500,
                name: "b".into(),
            },
        ];
        let layout = FileLayout::try_from_spans(spans).expect("合法双文件布局应通过校验");
        assert_eq!(layout.file_count(), 2);
        assert_eq!(layout.total_len(), 1500);
    }

    /// file_id 出现 0,2(缺 1)应 Err(FileIdGap)
    #[test]
    fn test_try_from_spans_rejects_file_id_gap() {
        let spans = vec![
            FileSpan {
                file_id: 0,
                global_offset: 0,
                len: 100,
                name: "a".into(),
            },
            FileSpan {
                file_id: 2,
                global_offset: 100,
                len: 100,
                name: "c".into(),
            },
        ];
        let err = FileLayout::try_from_spans(spans).expect_err("file_id 缺号 1 必须拒绝");
        assert!(
            matches!(err, LayoutError::FileIdGap),
            "缺号应返回 FileIdGap,实际: {err:?}"
        );
    }

    /// file_id 重复(0,0)应 Err(FileIdDuplicate)
    #[test]
    fn test_try_from_spans_rejects_duplicate_file_id() {
        let spans = vec![
            FileSpan {
                file_id: 0,
                global_offset: 0,
                len: 100,
                name: "a".into(),
            },
            FileSpan {
                file_id: 0,
                global_offset: 100,
                len: 100,
                name: "a2".into(),
            },
        ];
        let err = FileLayout::try_from_spans(spans).expect_err("重复 file_id=0 必须拒绝");
        assert!(
            matches!(err, LayoutError::FileIdDuplicate),
            "重复 file_id 应返回 FileIdDuplicate,实际: {err:?}"
        );
    }

    /// offset 出现空洞(span0=[0,500), span1=[1000,1500))应 Err(OffsetGap)
    #[test]
    fn test_try_from_spans_rejects_offset_gap() {
        let spans = vec![
            FileSpan {
                file_id: 0,
                global_offset: 0,
                len: 500,
                name: "a".into(),
            },
            FileSpan {
                file_id: 1,
                global_offset: 1000,
                len: 500,
                name: "b".into(),
            },
        ];
        let err = FileLayout::try_from_spans(spans).expect_err("offset 500-1000 空洞必须拒绝");
        assert!(
            matches!(err, LayoutError::OffsetGap),
            "offset 空洞应返回 OffsetGap,实际: {err:?}"
        );
    }

    /// offset 重叠(span0=[0,1000), span1=[500,1500))应 Err(OffsetOverlap)
    #[test]
    fn test_try_from_spans_rejects_offset_overlap() {
        let spans = vec![
            FileSpan {
                file_id: 0,
                global_offset: 0,
                len: 1000,
                name: "a".into(),
            },
            FileSpan {
                file_id: 1,
                global_offset: 500,
                len: 1000,
                name: "b".into(),
            },
        ];
        let err = FileLayout::try_from_spans(spans).expect_err("offset 500-1000 重叠必须拒绝");
        assert!(
            matches!(err, LayoutError::OffsetOverlap),
            "offset 重叠应返回 OffsetOverlap,实际: {err:?}"
        );
    }

    /// global_offset + len 溢出(offset=1, len=u64::MAX)应 Err(LenOverflow)
    #[test]
    fn test_try_from_spans_rejects_len_overflow() {
        let spans = vec![FileSpan {
            file_id: 0,
            global_offset: 1,
            len: u64::MAX,
            name: "overflow.bin".into(),
        }];
        let err = FileLayout::try_from_spans(spans).expect_err("offset+len 溢出必须拒绝");
        assert!(
            matches!(err, LayoutError::LenOverflow),
            "offset+len 溢出应返回 LenOverflow,实际: {err:?}"
        );
    }

    /// 单文件合法 span 应 Ok
    #[test]
    fn test_try_from_spans_accepts_single_file() {
        let spans = vec![FileSpan {
            file_id: 0,
            global_offset: 0,
            len: 8192,
            name: "single.bin".into(),
        }];
        let layout = FileLayout::try_from_spans(spans).expect("合法单文件布局应通过校验");
        assert_eq!(layout.file_count(), 1);
        assert_eq!(layout.total_len(), 8192);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// 分片 size 应始终等于 end - start + 1
        #[test]
        fn test_fragment_info_size_consistency(
            index in 0u32..1000,
            start in 0u64..u64::MAX / 2,
            size in 1u64..1024 * 1024 * 1024,
        ) {
            let end = start + size - 1;
            let frag = FragmentInfo {
                index,
                start,
                end,
                size,
                downloaded: 0,
                hash: None,
            };
            // 核心不变量: size == end - start + 1
            prop_assert_eq!(frag.size, frag.end - frag.start + 1);
            // end >= start（单字节分片时 end == start）
            prop_assert!(frag.end >= frag.start);
            // size 至少为 1
            prop_assert!(frag.size >= 1);
        }

        /// DownloadState 序列化/反序列化往返保持不变
        #[test]
        fn test_download_state_roundtrip(state in prop_oneof![
            Just(DownloadState::Pending),
            Just(DownloadState::Connecting),
            Just(DownloadState::Downloading),
            Just(DownloadState::Paused),
            Just(DownloadState::Resuming),
            Just(DownloadState::Verifying),
            Just(DownloadState::Completed),
            Just(DownloadState::Failed),
            Just(DownloadState::Cancelled),
        ]) {
            let json = serde_json::to_string(&state).unwrap();
            let deserialized: DownloadState = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(state, deserialized);
        }

        /// FileMetadata 序列化/反序列化往返保持关键字段一致
        #[test]
        fn test_file_metadata_roundtrip(
            file_name in "[a-zA-Z0-9_\\-]{1,50}",
            file_size in prop::option::of(0u64..1024 * 1024 * 1024),
            supports_range in proptest::bool::ANY,
        ) {
            let meta = FileMetadata {
                file_name: file_name.clone(),
                file_size,
                content_type: Some("application/octet-stream".into()),
                supports_range,
                etag: None,
                last_modified: None,
                file_layout: None,
                protocol_managed_storage: false,
                resolved_host: None,
            };
            let json = serde_json::to_string(&meta).unwrap();
            let deserialized: FileMetadata = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(deserialized.file_name, file_name);
            prop_assert_eq!(deserialized.file_size, file_size);
            prop_assert_eq!(deserialized.supports_range, supports_range);
        }

        /// FragmentInfo downloaded 不应超过 size
        #[test]
        fn test_fragment_downloaded_le_size(
            size in 1u64..1024 * 1024,
            downloaded in 0u64..1024 * 1024,
        ) {
            let clamped_downloaded = downloaded.min(size);
            let frag = FragmentInfo {
                index: 0,
                start: 0,
                end: size - 1,
                size,
                downloaded: clamped_downloaded,
                hash: None,
            };
            prop_assert!(frag.downloaded <= frag.size);
        }
    }
}
