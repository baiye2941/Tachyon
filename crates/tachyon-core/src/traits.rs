//! 核心 trait 定义
//!
//! 所有 crate 共享的公共接口抽象

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;

use crate::error::DownloadResult;
use crate::types::{FileMetadata, FragmentInfo, TaskId};

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
    fn download_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>>;

    /// 流式下载指定字节范围的数据
    ///
    /// 与 `download_range` 不同,此方法以流式方式返回数据块,
    /// 允许调用方边接收边写入存储,降低峰值内存占用。
    /// 调用方应使用 `StreamExt::next()` 逐块消费。
    fn download_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>>;

    /// 下载整个文件(不支持 Range 时使用)
    fn download_full(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>>;

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

pub trait Storage: Send + Sync {
    fn write_at(
        &self,
        offset: u64,
        data: Bytes,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + '_>>;

    fn read_at<'a>(
        &'a self,
        offset: u64,
        buf: &'a mut [u8],
    ) -> Pin<Box<dyn Future<Output = DownloadResult<usize>> + Send + 'a>>;

    fn sync(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    fn allocate(&self, size: u64) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;

    fn file_size(&self) -> Pin<Box<dyn Future<Output = DownloadResult<u64>> + Send + '_>>;

    fn close(&self) -> Pin<Box<dyn Future<Output = DownloadResult<()>> + Send + '_>>;
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
}

/// 常量时间字符串比较,防止时序侧信道攻击
///
/// 对所有字节进行 XOR 运算并累积差异,不提前返回,
/// 比较时间仅取决于较长字符串的长度,与内容无关。
fn constant_time_eq_str(a: &[u8], b: &[u8]) -> bool {
    // 长度不同也应保持恒定时间(不直接返回 false)
    let len = a.len().max(b.len());
    let mut diff: u8 = (a.len() != b.len()) as u8;
    for i in 0..len {
        let byte_a = a.get(i).copied().unwrap_or(0);
        let byte_b = b.get(i).copied().unwrap_or(0);
        diff |= byte_a ^ byte_b;
    }
    diff == 0
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

/// 分片下载 trait:单个分片的下载操作
///
/// 使用 `Pin<Box<dyn Future>>` 返回类型以满足 object-safe 条件,
/// 与 `Protocol` / `Storage` 等 trait 的设计风格保持一致,
/// 支持 `Arc<dyn FragmentDownloader>` 动态分发。
pub trait FragmentDownloader: Send + Sync {
    /// 下载单个分片
    fn download(
        &self,
        task_id: TaskId,
        fragment: FragmentInfo,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send + '_>>;

    /// 取消分片下载
    fn cancel(&self, task_id: TaskId, fragment_index: u32) -> DownloadResult<()>;
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

    impl Verifier for XorVerifier {
        fn compute_hash(&self, data: &[u8]) -> DownloadResult<String> {
            let xor = data.iter().fold(0u8, |acc, &b| acc ^ b);
            Ok(format!("{xor:02x}"))
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
    }
}
