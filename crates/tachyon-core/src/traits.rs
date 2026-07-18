//! 核心 trait 定义
//!
//! 所有 crate 共享的公共接口抽象

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use bytes::{Bytes, BytesMut};
use futures::Stream;

use crate::error::{DownloadError, DownloadResult};
use crate::types::{FileMetadata, ObjectIdentity};

/// 字节流类型别名
///
/// 用于 `download_range_stream` 的返回值,逐块产出 `DownloadResult<Bytes>`。
/// 调用方应使用 `StreamExt::next()` 逐块消费,避免将整个响应缓冲到内存。
pub type ByteStream = Pin<Box<dyn Stream<Item = DownloadResult<Bytes>> + Send>>;

/// 协议层 trait:负责与远程服务器通信
///
/// 使用 `Pin<Box<dyn Future>>` 返回类型以满足 object-safe 条件,
/// 支持 `Arc<dyn Protocol>` 动态分发。
///
/// 返回的 Future 生命周期为 `'static`,因为 `Arc<dyn Protocol>` 持有协议实例的所有权,
/// 调用方在 await 期间自行保证 self 和 url 的借用有效性。
pub trait Protocol: Send + Sync {
    /// 探测远程文件元数据(大小、是否支持 Range 等)
    fn probe(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<FileMetadata>> + Send>>;

    /// 下载指定字节范围的数据
    ///
    /// `identity` 为 probe/resume 建立的对象身份;HTTP 实现用于 `If-Range`。
    /// 无身份或非 HTTP 协议可传 `None`。
    fn download_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
        identity: Option<ObjectIdentity>,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>>;

    /// 流式下载指定字节范围的数据
    ///
    /// 与 `download_range` 不同,此方法以流式方式返回数据块,
    /// 允许调用方边接收边写入存储,降低峰值内存占用。
    /// 调用方应使用 `StreamExt::next()` 逐块消费。
    ///
    /// `identity` 语义同 `download_range`。
    fn download_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
        identity: Option<ObjectIdentity>,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>>;

    /// 下载整个文件(不支持 Range 时使用)
    fn download_full(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>>;

    /// 审计 HTTP-13:最近一次成功响应的 final host(重定向后 CDN)。
    ///
    /// 默认 `None`。HTTP 实现在 probe/range/full 后更新;引擎据此刷新 per-host 许可归属。
    fn last_resolved_host(&self) -> Option<String> {
        None
    }

    /// 清除已选中的源(用于重试时触发镜像轮换)
    ///
    /// 默认实现为空操作。`MirrorProtocol` 覆盖此方法以清除 probe 选中的源,
    /// 使下次下载尝试重新竞速所有镜像,避免重复使用已失败的源。
    fn clear_selected(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }

    /// 流式下载整个文件(不支持 Range 时使用)
    ///
    /// 与 `download_full` 不同,此方法以流式方式返回数据块,调用方边接收边写入,
    /// 峰值内存仅含单个 chunk,避免大文件整块进内存。
    ///
    /// 默认实现回退到 `download_full` 并包装为单块流,保证所有实现者无需改动即可工作;
    /// HTTP 等支持流式的协议应覆盖此方法以获得真正的低内存流式下载。
    fn download_full_stream(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
        let fut = self.download_full(url);
        Box::pin(async move {
            let data = fut.await?;
            Ok(Box::pin(futures::stream::once(async move { Ok(data) })) as ByteStream)
        })
    }
}

/// 异步存储后端抽象
///
/// 统一所有存储后端(TokioFile/WinFile/IoCp/IoUring/Memory)的读写接口,
/// 由 `tachyon-engine::DynStorage` 通过类型擦除(`ErasedStorage`)动态分发。
/// 添加新存储后端只需实现本 trait,无需修改引擎层枚举。
///
/// 本 trait 定义在 `tachyon-core` 而非 `tachyon-io`,因为它是跨 crate 共享的
/// 公共抽象(与 `Protocol`/`Verifier` 同层),且 `tachyon-core` 的测试 harness
/// `MemoryStorage` 需直接实现它,避免 `tachyon-io` 向 `tachyon-core` 反向依赖。
/// `tachyon-io` 通过 `pub use` 重导出本 trait 以保持 `tachyon_io::AsyncStorage`
/// 路径向后兼容。
pub trait AsyncStorage: Send + Sync {
    /// 写入 `Bytes` 数据,返回实际写入字节数
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>>;

    /// 写入 BytesMut 数据(避免 freeze() 产生额外复制)
    ///
    /// 默认实现复制一份 `Bytes` 后调用 `write_at`,后端应覆盖此方法
    /// 以直接从 `BytesMut` 内部缓冲区写入,避免复制/原子引用计数开销。
    ///
    /// 语义约定:
    /// - 方法返回实际写入的字节数 `n`,`data` 本身不会被修改。
    /// - 调用方需根据返回值自行 `data.advance(n)`,以处理短写循环。
    fn write_at_mut<'a>(
        &'a self,
        offset: u64,
        data: &'a mut BytesMut,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        // 默认实现通过复制构造 Bytes,避免将 &mut BytesMut 的借用带入 async 块。
        // 后端覆盖时应直接读取 data 的连续内存,且不消费 data。
        let frozen = Bytes::copy_from_slice(data);
        Box::pin(async move { self.write_at(offset, frozen).await })
    }

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>>;

    fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    fn allocate(&self, size: u64) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>>;

    fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    /// 对齐写入:自动处理 offset 和 data 的对齐填充。
    ///
    /// 为 WinFile NO_BUFFERING 和 IoUringStorage O_DIRECT 等需要对齐的后端
    /// 提供统一的对齐写入 API。默认实现通过填充零字节将 offset 向下对齐、
    /// data 向上对齐到 `alignment` 边界,然后委托给 `write_at`。
    ///
    /// - `alignment` 必须为 2 的幂(典型值:512 扇区 / 4096 页)
    /// - 返回实际写入的用户数据字节数(等于 `data.len()`)
    fn write_at_aligned<'a>(
        &'a self,
        offset: u64,
        data: &'a [u8],
        alignment: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
        Box::pin(async move {
            if alignment == 0 || !alignment.is_power_of_two() {
                return Err(DownloadError::Config(format!(
                    "alignment 必须为 2 的正整数幂, 实际值: {alignment}"
                )));
            }

            if data.is_empty() {
                return Ok(0);
            }

            let align_mask = alignment - 1;

            // 1. 将 offset 向下对齐到 alignment 边界
            let aligned_offset = offset & !align_mask;
            let front_pad = (offset - aligned_offset) as usize;

            // 2. 计算总填充大小(前端 + 数据 + 后端对齐)
            let total_len = front_pad + data.len();
            let padded_len = ((total_len as u64 + align_mask) & !align_mask) as usize;

            // 3. 构造对齐的写入缓冲区
            let mut padded = vec![0u8; padded_len];
            padded[front_pad..front_pad + data.len()].copy_from_slice(data);

            // 4. 委托给 write_at
            let written = self.write_at(aligned_offset, Bytes::from(padded)).await?;

            // 5. 返回用户数据的实际长度(而非填充后的长度)
            let user_written = written.saturating_sub(front_pad).min(data.len());
            Ok(user_written)
        })
    }
}

/// 流式哈希句柄,支持分块 update + finalize
///
/// 由 [`Verifier::new_hasher`] 创建,供下载/校验管线的流式哈希计算使用。
/// 生命周期:创建 → 多次 `update`(分块) → `finalize`(消耗 self 返回十六进制哈希)。
///
/// 与 `Verifier::compute_hash(&[u8])` 的一次性 API 互补:大文件无法整体加载进内存,
/// 下载管线需"边下边 update、写完再 finalize"的交错生命周期,由本 trait 承载。
pub trait StreamingHasher: Send {
    /// 追加数据块到哈希状态
    fn update(&mut self, data: &[u8]);

    /// 完成哈希计算,返回十六进制字符串(消耗 self)
    fn finalize(self: Box<Self>) -> String;
}

/// 校验层 trait:负责数据完整性校验
pub trait Verifier: Send + Sync {
    /// 计算数据的哈希值
    fn compute_hash(&self, data: &[u8]) -> DownloadResult<String>;

    /// 校验数据是否匹配预期哈希
    ///
    /// 使用常量时间比较防止时序侧信道攻击:
    /// 无论匹配与否,比较时间恒定(不因首个不匹配字节位置提前返回),
    /// 防止攻击者通过响应时间差异逐字符猜测哈希值。
    fn verify(&self, data: &[u8], expected_hash: &str) -> DownloadResult<()> {
        let actual = self.compute_hash(data)?;
        if constant_time_eq_str(actual.as_bytes(), expected_hash.as_bytes()) {
            Ok(())
        } else {
            Err(crate::error::DownloadError::ChecksumMismatch {
                expected: expected_hash.to_string(),
                actual,
            })
        }
    }

    /// 创建流式哈希句柄(供下载/校验管线分块计算)
    ///
    /// 后端应覆盖以返回原生流式实现(如 `blake3::Hasher`),
    /// 避免默认实现的缓冲全量数据再一次性 `compute_hash` 的内存放大。
    fn new_hasher(&self) -> Box<dyn StreamingHasher>;
}

/// 常量时间字符串比较,防止时序侧信道攻击
///
/// 基于 `subtle` crate(经 rustls 等审计)的 `ConstantTimeEq` 实现。
/// subtle 的 slice `ct_eq` 在长度不等时会短路返回 `Choice(0)`,
/// 这会通过时序泄漏长度差异;为保持与原手写实现一致的安全语义
/// (不因长度不同而提前返回),先取较短长度做等长内容比较,
/// 再以常量时间方式合并长度差异。最终比较时间仅取决于较短前缀长度,
/// 与内容无关。
fn constant_time_eq_str(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    // 取较短长度做等长比较,避免 subtle 在长度不等时的提前返回
    let min_len = a.len().min(b.len());
    let prefix_eq = a[..min_len].ct_eq(&b[..min_len]);
    // 长度差异编码为常量时间比较:用 1 字节表示长度是否相等,
    // 再与内容比较结果常量时间合并(subtle 的 & 是常量时间 AND)。
    // stable 不支持 Choice::new,故借 u8 的 ConstantTimeEq 构造 Choice。
    let len_byte = (a.len() == b.len()) as u8;
    let len_eq = [len_byte].ct_eq(&[1u8]);
    (prefix_eq & len_eq).into()
}

/// 下载任务执行器 trait:抽象下载任务的生命周期操作
///
/// 由 tachyon-engine 实现,供 tachyon-app 通过动态分发调用。
/// 避免 app 层直接依赖 `tachyon_engine::DownloadTask` 具体 struct,
/// 同时消除 `tachyon_core::traits::DownloadTask` 与 `tachyon_engine::DownloadTask`
/// 同名带来的语义混淆。
pub trait TaskRunner: Send + Sync {
    /// 注入引擎侧控制通道,engine 内部将 TaskCommand 翻译为 DownloadState
    fn set_control_rx(&mut self, rx: tokio::sync::watch::Receiver<crate::types::TaskCommand>);

    /// 注入已完成分片索引(断点续传)
    fn set_completed_fragments(&mut self, fragments: Vec<u32>);

    /// 注入未完整下载的分片及其已下载字节数(字节级断点续传)
    fn set_partial_fragments(&mut self, fragments: HashMap<u32, u64>);

    /// 注入断点快照对象身份(ETag/Last-Modified/size)
    fn set_resume_object_identity(&mut self, identity: Option<ObjectIdentity>);

    /// 注入断点快照 supports_range(Some(false) 时 probe 后强制整块路径)
    fn set_resume_supports_range(&mut self, supports_range: Option<bool>) {
        let _ = supports_range;
    }

    /// 注入分片进度发送端
    fn set_progress_sender(&mut self, tx: tokio::sync::mpsc::Sender<crate::FragmentProgress>);

    /// 注入用户重命名(可选):若为 `Some`,在 `probe()` 拿到元数据后会以此名覆盖
    /// `metadata.file_name`,使下游 `init_storage`/快照/UI 全部读到统一的文件名。
    /// 调用方负责传入已 sanitize 的合法文件名。
    fn set_preferred_file_name(&mut self, name: String);

    /// 探测远程文件元数据
    fn probe(&mut self)
    -> Pin<Box<dyn Future<Output = DownloadResult<&FileMetadata>> + Send + '_>>;

    /// 执行完整下载流程
    fn run(&mut self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    /// 获取已探测到的文件元数据
    fn metadata(&self) -> Option<&FileMetadata>;
}

// 为 Box<dyn TaskRunner> 提供默认转发实现,使 app 层可以持有 Box<dyn TaskRunner>
// 并直接以 &mut dyn TaskRunner 形式传给辅助函数,无需在每个调用点解引用。
impl<T: TaskRunner + ?Sized> TaskRunner for Box<T> {
    fn set_control_rx(&mut self, rx: tokio::sync::watch::Receiver<crate::types::TaskCommand>) {
        (**self).set_control_rx(rx)
    }

    fn set_completed_fragments(&mut self, fragments: Vec<u32>) {
        (**self).set_completed_fragments(fragments)
    }

    fn set_partial_fragments(&mut self, fragments: HashMap<u32, u64>) {
        (**self).set_partial_fragments(fragments)
    }

    fn set_resume_object_identity(&mut self, identity: Option<ObjectIdentity>) {
        (**self).set_resume_object_identity(identity)
    }

    fn set_resume_supports_range(&mut self, supports_range: Option<bool>) {
        (**self).set_resume_supports_range(supports_range)
    }

    fn set_progress_sender(&mut self, tx: tokio::sync::mpsc::Sender<crate::FragmentProgress>) {
        (**self).set_progress_sender(tx)
    }

    fn set_preferred_file_name(&mut self, name: String) {
        (**self).set_preferred_file_name(name)
    }

    fn probe(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<&FileMetadata>> + Send + '_>> {
        (**self).probe()
    }

    fn run(&mut self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
        (**self).run()
    }

    fn metadata(&self) -> Option<&FileMetadata> {
        (**self).metadata()
    }
}

/// 下载调度建议
///
/// 调度器根据带宽预测和文件特征返回的动态配置建议。
#[derive(Debug, Clone)]
pub struct ScheduleRecommendation {
    /// 建议的并发分片数
    pub concurrency: u32,
    /// 建议的分片大小(字节)
    pub fragment_size: u64,
    /// 带宽预测置信度(0.0 ~ 1.0)
    pub confidence: f64,
}

impl Default for ScheduleRecommendation {
    fn default() -> Self {
        Self {
            concurrency: 4,
            fragment_size: 4 * 1024 * 1024, // 4MB
            confidence: 0.0,
        }
    }
}

/// 下载调度器 trait:提供智能调度决策
///
/// 调度器负责:
/// - 基于带宽预测推荐并发度
/// - 根据网络状况动态调整分片大小
/// - 提供调度建议的置信度评估
pub trait DownloadScheduler: Send + Sync {
    /// 记录带宽观测值
    fn observe_bandwidth(&self, bytes_per_sec: u64);

    /// 记录链路往返时延(RTT)观测值
    ///
    /// 由 probe/下载阶段注入实测 RTT(如 TCP 握手 + TTFB),
    /// 用于修正 BDP(带宽延迟积)估计。默认实现为空(保持原行为),
    /// `AdaptiveDownloadScheduler` 覆盖为更新内部 RTT 状态。
    ///
    /// 高延迟链路(跨国 200ms+、卫星 600ms+)下,准确的 RTT 能避免
    /// 分片过小导致 TCP 窗口未打满、并发度不足导致管道空闲。
    fn observe_rtt(&self, _rtt: std::time::Duration) {}

    /// 获取调度建议
    ///
    /// 根据当前带宽预测、文件大小和配置约束,返回最优的并发度和分片大小建议。
    fn recommend(&self, file_size: u64, max_concurrency: u32) -> ScheduleRecommendation;

    /// 获取当前带宽预测(字节/秒)
    fn predicted_bandwidth(&self) -> u64;
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use tokio::sync::{mpsc, watch};

    use super::*;
    use crate::error::DownloadError;
    use crate::types::{FileMetadata, FragmentProgress, TaskCommand};

    /// 最小 Verifier 实现:逐字节 XOR 后转十六进制
    struct XorVerifier;

    /// XorVerifier 的流式句柄:缓冲所有数据后一次性 XOR(测试用,非生产)
    struct XorStreamingHasher(Vec<u8>);

    impl StreamingHasher for XorStreamingHasher {
        fn update(&mut self, data: &[u8]) {
            self.0.extend_from_slice(data);
        }
        fn finalize(self: Box<Self>) -> String {
            let xor = self.0.iter().fold(0u8, |acc, &b| acc ^ b);
            format!("{xor:02x}")
        }
    }

    impl Verifier for XorVerifier {
        fn compute_hash(&self, data: &[u8]) -> DownloadResult<String> {
            let xor = data.iter().fold(0u8, |acc, &b| acc ^ b);
            Ok(format!("{xor:02x}"))
        }
        fn new_hasher(&self) -> Box<dyn StreamingHasher> {
            Box::new(XorStreamingHasher(Vec::new()))
        }
    }

    #[test]
    fn test_verifier_compute_hash() {
        let verifier = XorVerifier;
        assert_eq!(verifier.compute_hash(b"").unwrap(), "00");
        assert_eq!(verifier.compute_hash(b"\x01\x01").unwrap(), "00");
        assert_eq!(verifier.compute_hash(b"\xff").unwrap(), "ff");
    }

    #[test]
    fn test_verifier_verify_success_and_failure() {
        let verifier = XorVerifier;
        let data = b"abc";
        let hash = verifier.compute_hash(data).unwrap();
        assert!(verifier.verify(data, &hash).is_ok());

        let result = verifier.verify(data, "00");
        assert!(
            matches!(
                result.unwrap_err(),
                DownloadError::ChecksumMismatch { expected, actual } if expected == "00" && actual == hash
            ),
            "校验失败应返回 ChecksumMismatch"
        );
    }

    #[test]
    fn test_constant_time_eq_str() {
        assert!(constant_time_eq_str(b"abc", b"abc"));
        assert!(!constant_time_eq_str(b"abc", b"abC"));
        assert!(!constant_time_eq_str(b"abc", b"ab"));
        assert!(!constant_time_eq_str(b"ab", b"abc"));
        assert!(constant_time_eq_str(b"", b""));
        assert!(!constant_time_eq_str(b"", b"a"));
        assert!(!constant_time_eq_str(b"a", b""));
        // 前缀相同但长度不同仍应返回 false
        assert!(!constant_time_eq_str(b"prefix", b"prefix-longer"));
        assert!(!constant_time_eq_str(b"prefix-longer", b"prefix"));
    }

    #[derive(Default)]
    struct MockTaskRunnerState {
        control_rx_set: bool,
        completed_fragments_set: bool,
        partial_fragments_set: bool,
        progress_sender_set: bool,
        preferred_file_name: Option<String>,
        probe_called: bool,
        run_called: bool,
    }

    struct MockTaskRunner {
        state: Arc<Mutex<MockTaskRunnerState>>,
        metadata: FileMetadata,
    }

    impl TaskRunner for MockTaskRunner {
        fn set_control_rx(&mut self, _rx: watch::Receiver<TaskCommand>) {
            self.state.lock().unwrap().control_rx_set = true;
        }

        fn set_completed_fragments(&mut self, _fragments: Vec<u32>) {
            self.state.lock().unwrap().completed_fragments_set = true;
        }

        fn set_partial_fragments(&mut self, _fragments: HashMap<u32, u64>) {
            self.state.lock().unwrap().partial_fragments_set = true;
        }

        fn set_resume_object_identity(&mut self, _identity: Option<ObjectIdentity>) {}

        fn set_progress_sender(&mut self, _tx: mpsc::Sender<FragmentProgress>) {
            self.state.lock().unwrap().progress_sender_set = true;
        }

        fn set_preferred_file_name(&mut self, name: String) {
            self.state.lock().unwrap().preferred_file_name = Some(name);
        }

        fn probe(
            &mut self,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<&FileMetadata>> + Send + '_>> {
            self.state.lock().unwrap().probe_called = true;
            Box::pin(async { Ok(&self.metadata) })
        }

        fn run(&mut self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            self.state.lock().unwrap().run_called = true;
            Box::pin(async { Ok(()) })
        }

        fn metadata(&self) -> Option<&FileMetadata> {
            Some(&self.metadata)
        }
    }

    #[tokio::test]
    async fn test_task_runner_all_methods_and_box_forwarding() {
        let state = Arc::new(Mutex::new(MockTaskRunnerState::default()));
        let metadata = FileMetadata {
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
        let mock = MockTaskRunner {
            state: Arc::clone(&state),
            metadata,
        };

        let mut runner: Box<dyn TaskRunner> = Box::new(mock);

        let (_tx, rx) = watch::channel(TaskCommand::Start);
        runner.set_control_rx(rx);
        runner.set_completed_fragments(vec![0, 1]);
        runner.set_partial_fragments(HashMap::from([(2, 100)]));
        let (progress_tx, _progress_rx) = mpsc::channel::<FragmentProgress>(1);
        runner.set_progress_sender(progress_tx);
        runner.set_preferred_file_name("renamed.bin".into());

        let meta_ref = runner.probe().await.unwrap();
        assert_eq!(meta_ref.file_name, "test.bin");
        runner.run().await.unwrap();

        let s = state.lock().unwrap();
        assert!(s.control_rx_set);
        assert!(s.completed_fragments_set);
        assert!(s.partial_fragments_set);
        assert!(s.progress_sender_set);
        assert_eq!(s.preferred_file_name.as_deref(), Some("renamed.bin"));
        assert!(s.probe_called);
        assert!(s.run_called);
        // 覆盖 Box<dyn TaskRunner>::metadata() 转发(L322-324)
        assert!(runner.metadata().is_some());
        assert_eq!(runner.metadata().unwrap().file_name, "test.bin");
    }

    #[test]
    fn test_xor_verifier_and_streaming_hasher() {
        // 覆盖 XorVerifier 和 XorStreamingHasher(L387-393, 401-403)
        let verifier = XorVerifier;
        let hash = verifier.compute_hash(b"abc").unwrap();
        // 'a' ^ 'b' ^ 'c' = 0x61 ^ 0x62 ^ 0x63 = 0x60
        assert_eq!(hash, "60");

        // 流式哈希应与一次性哈希一致
        let mut hasher = verifier.new_hasher();
        hasher.update(b"a");
        hasher.update(b"bc");
        let streaming_hash = hasher.finalize();
        assert_eq!(streaming_hash, hash);
    }

    // ── AsyncStorage trait 默认实现测试 ──────────────────────────────

    /// 最小存储后端:仅在内存中保存数据,用于测试 trait 默认实现
    struct InMemStorage {
        data: std::sync::Mutex<Vec<u8>>,
    }

    impl InMemStorage {
        fn new() -> Self {
            Self {
                data: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl AsyncStorage for InMemStorage {
        fn write_at<'a>(
            &'a self,
            offset: u64,
            data: Bytes,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            Box::pin(async move {
                let mut guard = self.data.lock().unwrap();
                let off = offset as usize;
                let need = off + data.len();
                if guard.len() < need {
                    guard.resize(need, 0);
                }
                guard[off..off + data.len()].copy_from_slice(&data);
                Ok(data.len())
            })
        }

        fn read_at<'a>(
            &'a self,
            offset: u64,
            buf: &'a mut [u8],
        ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>> {
            Box::pin(async move {
                let guard = self.data.lock().unwrap();
                let off = offset as usize;
                if off >= guard.len() {
                    return Ok(0);
                }
                let end = (off + buf.len()).min(guard.len());
                let n = end - off;
                buf[..n].copy_from_slice(&guard[off..end]);
                Ok(n)
            })
        }

        fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn allocate(
            &self,
            size: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async move {
                let mut guard = self.data.lock().unwrap();
                guard.resize(size as usize, 0);
                Ok(())
            })
        }

        fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>> {
            Box::pin(async move {
                let guard = self.data.lock().unwrap();
                Ok(guard.len() as u64)
            })
        }

        fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn test_write_at_mut_default_impl_copies_then_delegates() {
        // 覆盖 AsyncStorage::write_at_mut 默认实现(L115-124)
        // 默认实现复制 BytesMut → Bytes,然后委托 write_at
        let storage = InMemStorage::new();
        let mut data = bytes::BytesMut::from(&b"hello world"[..]);
        let n = storage.write_at_mut(0, &mut data).await.unwrap();
        assert_eq!(n, 11);
        // data 未被消费(仍可读)
        assert_eq!(&data[..], b"hello world");
        // 数据已写入存储
        let mut buf = [0u8; 11];
        let read = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(read, 11);
        assert_eq!(&buf, b"hello world");
    }

    #[tokio::test]
    async fn test_write_at_aligned_invalid_alignment() {
        // 覆盖 alignment=0 和非 2 幂错误分支(L155-159)
        let storage = InMemStorage::new();
        let result = storage.write_at_aligned(0, b"data", 0).await;
        assert!(result.is_err());

        let result = storage.write_at_aligned(0, b"data", 3).await; // 3 非 2 幂
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_write_at_aligned_empty_data() {
        // 覆盖 data.is_empty() 短路返回 0(L161-163)
        let storage = InMemStorage::new();
        let n = storage.write_at_aligned(0, b"", 512).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn test_write_at_aligned_aligned_offset() {
        // 覆盖 offset 已对齐的正常路径(L165-184)
        let storage = InMemStorage::new();
        let n = storage
            .write_at_aligned(512, b"aligned", 512)
            .await
            .unwrap();
        assert_eq!(n, 7);
        let mut buf = [0u8; 7];
        let read = storage.read_at(512, &mut buf).await.unwrap();
        assert_eq!(read, 7);
        assert_eq!(&buf, b"aligned");
    }

    #[tokio::test]
    async fn test_write_at_aligned_unaligned_offset() {
        // 覆盖 offset 未对齐时的前端填充路径(L168-177)
        let storage = InMemStorage::new();
        // offset=100, alignment=512 → aligned_offset=0, front_pad=100
        let n = storage.write_at_aligned(100, b"test", 512).await.unwrap();
        assert_eq!(n, 4);
        let mut buf = [0u8; 4];
        let read = storage.read_at(100, &mut buf).await.unwrap();
        assert_eq!(read, 4);
        assert_eq!(&buf, b"test");
    }

    #[tokio::test]
    async fn test_write_at_aligned_partial_short_write() {
        // 覆盖 written.saturating_sub(front_pad).min(data.len()) 路径(L183)
        let storage = InMemStorage::new();
        // offset=100, alignment=512, data="abc"
        let n = storage.write_at_aligned(100, b"abc", 512).await.unwrap();
        assert_eq!(n, 3, "应返回用户数据长度 3");
    }

    #[test]
    fn test_schedule_recommendation_default() {
        // 覆盖 ScheduleRecommendation::default(L341-347)
        let rec = ScheduleRecommendation::default();
        assert!(rec.fragment_size > 0, "默认 fragment_size 应 > 0");
    }

    #[tokio::test]
    async fn test_inmem_storage_full_lifecycle() {
        // 覆盖 InMemStorage 的 read_at/sync/allocate/file_size/close 全部方法
        // (这些方法被定义但未直接测试,只通过 write_at_mut 间接调用)
        let storage = InMemStorage::new();
        // allocate
        storage.allocate(100).await.unwrap();
        assert_eq!(storage.file_size().await.unwrap(), 100);
        // write + read
        let mut data = bytes::BytesMut::from(&b"hello"[..]);
        storage.write_at_mut(0, &mut data).await.unwrap();
        let mut buf = [0u8; 5];
        let n = storage.read_at(0, &mut buf).await.unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf, b"hello");
        // read past end
        let n = storage.read_at(200, &mut buf).await.unwrap();
        assert_eq!(n, 0, "越界读应返回 0");
        // sync + close(仅验证不 panic)
        storage.sync().await.unwrap();
        storage.close().await.unwrap();
    }
}
