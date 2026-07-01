//! 多镜像源 Protocol 适配器(多源并发 + least-in-flight 调度)
//!
//! 包装主源和备用源列表,采用多源并发 + least-in-flight 选源策略:
//! - **probe**: 并行竞速所有源的 HEAD 探测,记录成功的源集合(probe_ok)
//! - **download**: 每次调用从 probe_ok 的源里选"在途分片数最少"的源
//!   (least-in-flight),快源完成快→在途少→多被选→多干(隐式 work-stealing,
//!   聚合多源带宽)。失败源惩罚性保留在途数,使重试优先选其他源。
//!
//! 与旧"单源 selected 固定 + 失败全源竞速"相比,多源并发聚合带宽,
//! 不浪费带宽(每分片一个源拉),快源多干消尾延迟。

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use tachyon_core::traits::Protocol;
use tachyon_core::types::FileMetadata;
use tachyon_core::{ByteStream, DownloadError, DownloadResult};

/// 源 URL + Protocol 对
type Source = (String, Arc<dyn Protocol>);

/// 源质量统计(轻量,engine 内自管)
///
/// 记录每源的成功/失败次数、累计字节/耗时,派生 quality(0~1,越高越好)。
/// 选源时用 `in_flight / (quality + ε)` 加权:质量高的源允许更多在途。
#[derive(Debug, Clone, Default)]
struct SourceStats {
    success: u64,
    fail: u64,
    total_bytes: u64,
    total_duration_ns: u128,
}

impl SourceStats {
    /// 稳定性:成功次数 / 总尝试次数(无数据时 0.5 中性)
    fn stability(&self) -> f64 {
        let total = self.success + self.fail;
        if total == 0 {
            0.5
        } else {
            self.success as f64 / total as f64
        }
    }

    /// 平均带宽(bps;无数据时 0)
    fn avg_bandwidth_bps(&self) -> f64 {
        if self.total_duration_ns == 0 {
            return 0.0;
        }
        let secs = self.total_duration_ns as f64 / 1_000_000_000.0;
        if secs > 0.0 {
            self.total_bytes as f64 * 8.0 / secs
        } else {
            0.0
        }
    }

    /// 综合质量(0~1,越高越好)。
    ///
    /// 修复 BUG-C:bandwidth 权重 0.7(主导),stability 0.3(辅助)。
    /// 带宽归一化用 10Mbps 基准。无数据返回 0.5。
    /// 若总耗时 < 1ms(小数据/单 chunk),bandwidth 无意义,只用 stability。
    fn quality(&self) -> f64 {
        let total = self.success + self.fail;
        if total == 0 {
            return 0.5;
        }
        let stability = self.stability();
        // 极短耗时(单 chunk / 小数据):bandwidth 无意义,只用 stability
        if self.total_duration_ns < 1_000_000 {
            return stability * 0.5; // 0~0.5,低于未测源 0.5(已测且慢的不优于未测)
        }
        // 带宽归一化:10Mbps 为满分基准
        let bandwidth_score = (self.avg_bandwidth_bps() / (10.0 * 1024.0 * 1024.0)).min(1.0);
        stability * 0.3 + bandwidth_score * 0.7
    }

    /// 记录一次成功下载
    fn record_success(&mut self, bytes: u64, duration_ns: u128) {
        self.success += 1;
        self.total_bytes += bytes;
        self.total_duration_ns += duration_ns;
    }

    /// 记录一次失败
    fn record_failure(&mut self) {
        self.fail += 1;
    }
}

/// ByteStream 统计包装器:记录字节/耗时,流结束时更新源 stats + 递减 in_flight
///
/// - poll_next 返回 EOF(Ready(None)):记 success(bytes, duration)+ 递减 in_flight
/// - poll_next 返回 Err:记 fail + 递减 in_flight
/// - Drop(未正常结束,如 worker abort):递减 in_flight(流不在途了)但不更新 stats
///   (避免误惩罚用户取消;in_flight 递减是事实,stats 不记是保守)
///
/// 修复 BUG-A:in_flight 延迟到流真正结束时递减,而非 download_range_stream 返回时。
struct StatsStream {
    inner: ByteStream,
    source_idx: usize,
    stats: Arc<std::sync::Mutex<Vec<SourceStats>>>,
    in_flight: Arc<std::sync::Mutex<Vec<usize>>>,
    start: Option<std::time::Instant>, // 首字节时记录(修复 OPT-6:不含排队等待)
    bytes_seen: u64,
    finished: bool,
}

impl StatsStream {
    fn wrap(
        inner: ByteStream,
        source_idx: usize,
        stats: Arc<std::sync::Mutex<Vec<SourceStats>>>,
        in_flight: Arc<std::sync::Mutex<Vec<usize>>>,
    ) -> ByteStream {
        Box::pin(Self {
            inner,
            source_idx,
            stats,
            in_flight,
            // start 在 wrap 时记录:包含"流创建到 EOF"全程
            // (含 download_fn 的 sleep/网络等待,反映真实下载耗时)
            start: Some(std::time::Instant::now()),
            bytes_seen: 0,
            finished: false,
        }) as ByteStream
    }

    /// 流正常结束(EOF)时记 success + 递减 in_flight
    fn record_success(&mut self) {
        if let Some(start) = self.start {
            let duration_ns = start.elapsed().as_nanos();
            if let Ok(mut stats) = self.stats.lock() {
                stats[self.source_idx].record_success(self.bytes_seen, duration_ns);
            }
        }
        self.decrement_in_flight();
    }

    /// 流出错时记 fail + 递减 in_flight
    fn record_fail(&mut self) {
        if let Ok(mut stats) = self.stats.lock() {
            stats[self.source_idx].record_failure();
        }
        self.decrement_in_flight();
    }

    /// 递减源在途数(幂等,finished 防重复)
    fn decrement_in_flight(&self) {
        if let Ok(mut inflight) = self.in_flight.lock()
            && inflight[self.source_idx] > 0
        {
            inflight[self.source_idx] -= 1;
        }
    }
}

impl futures::Stream for StatsStream {
    type Item = DownloadResult<Bytes>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut(); // StatsStream: Unpin(所有字段 Unpin)
        match this.inner.as_mut().poll_next(cx) {
            std::task::Poll::Ready(None) => {
                if !this.finished {
                    this.finished = true;
                    this.record_success();
                }
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Ready(Some(Ok(bytes))) => {
                this.bytes_seen += bytes.len() as u64;
                std::task::Poll::Ready(Some(Ok(bytes)))
            }
            std::task::Poll::Ready(Some(Err(e))) => {
                if !this.finished {
                    this.finished = true;
                    this.record_fail();
                }
                std::task::Poll::Ready(Some(Err(e)))
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl Drop for StatsStream {
    fn drop(&mut self) {
        // Drop(abort/取消)时:若未正常 EOF/Err,仍递减 in_flight(流确实不在途了)
        // 但不更新 stats(避免误惩罚用户取消;stats 只在 EOF/Err 记)
        if !self.finished {
            self.decrement_in_flight();
        }
    }
}

/// 多镜像源 Protocol 适配器(多源并发 + least-in-flight)
pub(crate) struct MirrorProtocol {
    /// 所有源(index 0=primary, 1..N=mirrors)
    sources: Vec<Source>,
    /// probe 成功的源 index 集合(download 只从中选;空集则用全部源)
    probe_ok: Arc<Mutex<HashSet<usize>>>,
    /// 每源在途分片数(与 sources 等长,least-in-flight 选源依据)
    in_flight: Arc<std::sync::Mutex<Vec<usize>>>,
    /// 每源质量统计(与 sources 等长,选源加权 + stability 回填)
    /// 用 std::sync::Mutex 而非 tokio::sync::Mutex:StatsStream::poll_next 是同步的,
    /// 且 stats 更新极短(计数递增),不会阻塞 runtime。
    stats: Arc<std::sync::Mutex<Vec<SourceStats>>>,
}

impl MirrorProtocol {
    pub(crate) fn new(
        primary: Arc<dyn Protocol>,
        mirrors: Vec<(String, Arc<dyn Protocol>)>,
    ) -> Self {
        let mut sources = vec![(String::new(), primary)];
        sources.extend(mirrors);
        let n = sources.len();
        Self {
            sources,
            probe_ok: Arc::new(Mutex::new(HashSet::new())),
            in_flight: Arc::new(std::sync::Mutex::new(vec![0; n])),
            stats: Arc::new(std::sync::Mutex::new(vec![SourceStats::default(); n])),
        }
    }

    /// 清除 probe 结果 + 重置选源状态(修复 BUG-B:不清 in_flight 导致失败源永久饿死)
    pub(crate) async fn clear_selected(&self) {
        *self.probe_ok.lock().await = HashSet::new();
        // 重置 in_flight:重试前所有源在途数归零(避免跨调用累积)
        if let Ok(mut inflight) = self.in_flight.lock() {
            for v in inflight.iter_mut() {
                *v = 0;
            }
        }
    }

    /// least-in-flight + 质量加权下载:选源 → 下载 → 成功递减;失败则内部 fallback
    ///
    /// 选源:in_flight / (quality + ε),质量高的源允许更多在途(快源多干)。
    /// 失败源惩罚性保留在途数(不递减),使后续选源优先选其他源。
    /// 遍历所有候选源直到成功或全失败(对调用方透明)。
    /// 成功返回 (数据, 选中源 idx)(idx 供 stream 路径包 StatsStream 用)。
    async fn download_via_least_in_flight<T: Send + 'static>(
        sources: Vec<Source>,
        probe_ok: Arc<Mutex<HashSet<usize>>>,
        in_flight: Arc<std::sync::Mutex<Vec<usize>>>,
        stats: Arc<std::sync::Mutex<Vec<SourceStats>>>,
        url: &str,
        download_fn: impl Fn(
            Arc<dyn Protocol>,
            String,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<T>> + Send>>
        + Clone
        + Send
        + 'static,
        error_label: &str,
    ) -> DownloadResult<(T, usize)> {
        // 候选源 index 列表:probe_ok 优先,全部源兜底
        // 修复 BUG-D:排序保证确定性 tie-break(HashSet 迭代顺序随机导致 flaky)
        // 修复 BUG-H 副作用:probe 首成功即返回时,后台 probe_ok 可能未补全,
        //   download 时 probe_ok 的候选失败后需全部源兜底
        let mut candidates: Vec<usize> = {
            let ok = probe_ok.lock().await;
            if ok.is_empty() {
                (0..sources.len()).collect()
            } else {
                // probe_ok 优先,再补全部源(去重)
                let mut v: Vec<usize> = ok.iter().copied().collect();
                for i in 0..sources.len() {
                    if !v.contains(&i) {
                        v.push(i);
                    }
                }
                v
            }
        };
        candidates.sort_unstable();
        candidates.dedup();

        let mut last_err = None;
        // 遍历候选源,每次选加权最小的(in_flight 主导 + quality 辅助)
        for _ in 0..candidates.len() {
            let (idx, src_url, proto) = {
                let mut inflight = in_flight.lock().unwrap();
                // 快照各源 quality(std Mutex,持锁极短,不跨 await)
                let qualities: Vec<f64> =
                    stats.lock().unwrap().iter().map(|s| s.quality()).collect();
                // 加权(修复 BUG-A/C):加性公式 inflight*W1 + (1-quality)*W2
                // inflight=0 时按 quality 排序(quality 高→(1-quality)小→优先);
                // inflight>0 时在途数主导(负载均衡)。
                // W1=10000(在途数权重高),W2=1000(质量权重)
                let pick = candidates
                    .iter()
                    .copied()
                    .filter(|&i| inflight[i] < usize::MAX)
                    .min_by(|&a, &b| {
                        let sa = inflight[a] as f64 * 10000.0 + (1.0 - qualities[a]) * 1000.0;
                        let sb = inflight[b] as f64 * 10000.0 + (1.0 - qualities[b]) * 1000.0;
                        sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
                    });
                let Some(pick) = pick else { break };
                // 选源和递增在同一锁内(修复 OPT-3 TOCTOU)
                inflight[pick] += 1;
                let actual_url = if pick == 0 {
                    url.to_string()
                } else {
                    sources[pick].0.clone()
                };
                (pick, actual_url, sources[pick].1.clone())
            };

            match download_fn(proto, src_url).await {
                Ok(data) => {
                    // 不在此递减 in_flight(修复 BUG-A):
                    // - Bytes 路径:调用方(download_range/download_full)拿 (data, idx) 后递减
                    // - stream 路径:StatsStream 在流 EOF/Err/Drop 时递减
                    return Ok((data, idx));
                }
                Err(e) => {
                    // 失败:递减 in_flight(不累积,修复 BUG-B)+ 记 fail stats(降 quality)
                    let mut inflight = in_flight.lock().unwrap();
                    if inflight[idx] > 0 {
                        inflight[idx] -= 1;
                    }
                    tracing::info!(error = %e, source_idx = idx, error_label, "源下载失败,切换下一源");
                    stats.lock().unwrap()[idx].record_failure();
                    last_err = Some(e);
                }
            }
        }
        Err(last_err
            .unwrap_or_else(|| DownloadError::Protocol(format!("所有源均失败{error_label}"))))
    }
}

impl Protocol for MirrorProtocol {
    fn clear_selected(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move { self.clear_selected().await })
    }

    fn probe(
        &self,
        url: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<FileMetadata>> + Send>>
    {
        let sources = self.sources.clone();
        let probe_ok = self.probe_ok.clone();
        let url = url.to_string();
        Box::pin(async move {
            if sources.len() == 1 {
                // 单源(无镜像):直接 probe
                let result = sources[0].1.probe(&url).await;
                if result.is_ok() {
                    probe_ok.lock().await.insert(0);
                }
                return result;
            }

            // 并行 probe 所有源,首成功即返回(修复 BUG-H:不等慢源)
            // 剩余源后台 spawn,完成时补全 probe_ok(least-in-flight 后续可用更多源)
            let mut set = JoinSet::new();
            for (i, (src_url, proto)) in sources.iter().enumerate() {
                let p = proto.clone();
                let u = if i == 0 { url.clone() } else { src_url.clone() };
                set.spawn(async move { (i, p.probe(&u).await) });
            }

            let mut first_ok_meta: Option<FileMetadata> = None;
            let mut first_ok_idx: Option<usize> = None;
            let mut last_err = None;
            // 第一阶段:等到首个成功或全部失败
            while let Some(result) = set.join_next().await {
                match result {
                    Ok((idx, Ok(meta))) => {
                        first_ok_meta = Some(meta);
                        first_ok_idx = Some(idx);
                        break; // 首成功即返回
                    }
                    Ok((_idx, Err(e))) => last_err = Some(e),
                    Err(e) => {
                        last_err = Some(DownloadError::Io(std::io::Error::other(e.to_string())))
                    }
                }
            }

            let (Some(meta), Some(idx)) = (first_ok_meta, first_ok_idx) else {
                return Err(
                    last_err.unwrap_or_else(|| DownloadError::Protocol("所有源探测均失败".into()))
                );
            };

            // 记录首个成功源
            probe_ok.lock().await.insert(idx);

            // 第二阶段:剩余 probe 任务后台 spawn,完成时补全 probe_ok
            // set 还持有未完成的 probe,detach 让它们继续跑
            let probe_ok_bg = probe_ok.clone();
            tokio::spawn(async move {
                while let Some(result) = set.join_next().await {
                    if let Ok((idx, Ok(_))) = result {
                        probe_ok_bg.lock().await.insert(idx);
                    }
                }
            });

            Ok(meta)
        })
    }

    fn download_range(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
        let sources = self.sources.clone();
        let probe_ok = self.probe_ok.clone();
        let in_flight = self.in_flight.clone();
        let stats = self.stats.clone();
        let url = url.to_string();
        Box::pin(async move {
            let (data, idx) = Self::download_via_least_in_flight(
                sources,
                probe_ok,
                in_flight.clone(),
                stats,
                &url,
                move |proto, u| proto.download_range(&u, start, end),
                "",
            )
            .await?;
            // Bytes 路径:data 已就绪,立即递减 in_flight(stream 路径由 StatsStream 递减)
            if let Ok(mut inflight) = in_flight.lock()
                && inflight[idx] > 0
            {
                inflight[idx] -= 1;
            }
            Ok(data)
        })
    }

    fn download_range_stream(
        &self,
        url: &str,
        start: u64,
        end: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<ByteStream>> + Send>>
    {
        let sources = self.sources.clone();
        let probe_ok = self.probe_ok.clone();
        let in_flight = self.in_flight.clone();
        let stats = self.stats.clone();
        let url = url.to_string();
        Box::pin(async move {
            let (stream, idx) = Self::download_via_least_in_flight(
                sources,
                probe_ok,
                in_flight.clone(),
                stats.clone(),
                &url,
                move |proto, u| proto.download_range_stream(&u, start, end),
                "(流式)",
            )
            .await?;
            // 包 StatsStream:流 EOF/Err/Drop 时递减 in_flight + 记 stats(修复 BUG-A)
            Ok(StatsStream::wrap(stream, idx, stats, in_flight))
        })
    }

    fn download_full(
        &self,
        url: &str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = DownloadResult<Bytes>> + Send>> {
        let sources = self.sources.clone();
        let probe_ok = self.probe_ok.clone();
        let in_flight = self.in_flight.clone();
        let stats = self.stats.clone();
        let url = url.to_string();
        Box::pin(async move {
            let (data, idx) = Self::download_via_least_in_flight(
                sources,
                probe_ok,
                in_flight.clone(),
                stats,
                &url,
                move |proto, u| proto.download_full(&u),
                "(全量)",
            )
            .await?;
            // Bytes 路径:立即递减 in_flight
            if let Ok(mut inflight) = in_flight.lock()
                && inflight[idx] > 0
            {
                inflight[idx] -= 1;
            }
            Ok(data)
        })
    }

    /// 覆写 download_full_stream(修复 BUG-J:默认实现走 download_full 不记 stats)
    ///
    /// 走 download_via_least_in_flight 选源 + StatsStream 包裹(与 download_range_stream 对称)。
    fn download_full_stream(
        &self,
        url: &str,
    ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
        let sources = self.sources.clone();
        let probe_ok = self.probe_ok.clone();
        let in_flight = self.in_flight.clone();
        let stats = self.stats.clone();
        let url = url.to_string();
        Box::pin(async move {
            let (stream, idx) = Self::download_via_least_in_flight(
                sources,
                probe_ok,
                in_flight.clone(),
                stats.clone(),
                &url,
                move |proto, u| proto.download_full_stream(&u),
                "(全量流式)",
            )
            .await?;
            Ok(StatsStream::wrap(stream, idx, stats, in_flight))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Bytes;
    use futures::StreamExt;

    use super::MirrorProtocol;
    use tachyon_core::traits::Protocol;
    use tachyon_core::types::FileMetadata;
    use tachyon_core::{ByteStream, DownloadError, DownloadResult};

    #[derive(Clone)]
    struct MockProtocol {
        probe_delay: Duration,
        probe_meta: Result<FileMetadata, String>,
        download_data: Result<Bytes, String>,
        /// 若设置,download_* 收到不匹配此 URL 的请求则失败(用于验证竞速选中后用对 URL)
        expected_url: Option<String>,
        /// download 延迟(模拟源速度差异,用于 least-in-flight 测试)
        download_delay: Duration,
        /// 被选计数器(共享,记录该源被调 download_* 的次数,验证 least-in-flight 分配)
        select_counter: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl MockProtocol {
        fn new() -> Self {
            Self {
                probe_delay: Duration::ZERO,
                probe_meta: Ok(FileMetadata {
                    file_name: "mock".into(),
                    file_size: Some(100),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                    file_layout: None,
                }),
                download_data: Ok(Bytes::from_static(b"mock")),
                expected_url: None,
                download_delay: Duration::ZERO,
                select_counter: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn with_probe_delay(mut self, delay: Duration) -> Self {
            self.probe_delay = delay;
            self
        }

        fn with_expected_url(mut self, url: impl Into<String>) -> Self {
            self.expected_url = Some(url.into());
            self
        }

        fn with_probe_meta(mut self, meta: Result<FileMetadata, String>) -> Self {
            self.probe_meta = meta;
            self
        }

        fn with_download_data(mut self, data: Result<Bytes, String>) -> Self {
            self.download_data = data;
            self
        }

        /// 设置 download 延迟(模拟源速度)
        fn with_download_delay(mut self, delay: Duration) -> Self {
            self.download_delay = delay;
            self
        }
    }

    impl Protocol for MockProtocol {
        fn probe(
            &self,
            _url: &str,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<FileMetadata>> + Send>> {
            let delay = self.probe_delay;
            let result = self.probe_meta.clone();
            Box::pin(async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                result.map_err(DownloadError::Protocol)
            })
        }

        fn download_range(
            &self,
            url: &str,
            _start: u64,
            _end: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
            let result = self.download_data.clone();
            let expected = self.expected_url.clone();
            let url = url.to_string();
            Box::pin(async move {
                if let Some(exp) = expected
                    && exp != url
                {
                    return Err(DownloadError::Protocol(format!(
                        "URL 不匹配: 期望 {exp}, 实际 {url}"
                    )));
                }
                result.map_err(DownloadError::Protocol)
            })
        }

        fn download_range_stream(
            &self,
            _url: &str,
            _start: u64,
            _end: u64,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<ByteStream>> + Send>> {
            let result = self.download_data.clone();
            let delay = self.download_delay;
            let counter = self.select_counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let data = result.map_err(DownloadError::Protocol)?;
                // delay 放到首 chunk 内(模拟传输耗时,StatsStream 能测到 duration)
                let stream = futures::stream::once(async move {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    Ok(data)
                });
                Ok(Box::pin(stream) as ByteStream)
            })
        }

        fn download_full(
            &self,
            url: &str,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<Bytes>> + Send>> {
            let result = self.download_data.clone();
            let expected = self.expected_url.clone();
            let url = url.to_string();
            Box::pin(async move {
                if let Some(exp) = expected
                    && exp != url
                {
                    return Err(DownloadError::Protocol(format!(
                        "URL 不匹配: 期望 {exp}, 实际 {url}"
                    )));
                }
                result.map_err(DownloadError::Protocol)
            })
        }
    }

    #[tokio::test]
    async fn test_probe_selects_fastest_mirror() {
        tokio::time::pause();

        let slow_primary = Arc::new(
            MockProtocol::new()
                .with_probe_delay(Duration::from_secs(10))
                .with_probe_meta(Ok(FileMetadata {
                    file_name: "primary".into(),
                    file_size: Some(100),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                    file_layout: None,
                })),
        );

        let fast_mirror = Arc::new(
            MockProtocol::new()
                .with_probe_delay(Duration::from_millis(100))
                .with_probe_meta(Ok(FileMetadata {
                    file_name: "mirror".into(),
                    file_size: Some(200),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                    file_layout: None,
                })),
        );

        let mirror_protocol = MirrorProtocol::new(
            slow_primary,
            vec![("http://mirror.com/file".into(), fast_mirror)],
        );

        let result = mirror_protocol.probe("http://primary.com/file").await;
        assert!(result.is_ok(), "probe 应返回最快镜像的结果");
        assert_eq!(
            result.unwrap().file_size,
            Some(200),
            "应选择最快镜像(file_size=200)"
        );
    }

    #[tokio::test]
    async fn test_clear_selected_resets_probe_choice() {
        let primary = Arc::new(MockProtocol::new().with_probe_meta(Ok(FileMetadata {
            file_name: "primary".into(),
            file_size: Some(100),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
        })));

        let mirror = Arc::new(MockProtocol::new().with_probe_meta(Ok(FileMetadata {
            file_name: "mirror".into(),
            file_size: Some(200),
            content_type: None,
            supports_range: true,
            etag: None,
            last_modified: None,
            file_layout: None,
        })));

        let mirror_protocol =
            MirrorProtocol::new(primary, vec![("http://mirror.com/file".into(), mirror)]);

        let meta = mirror_protocol
            .probe("http://primary.com/file")
            .await
            .unwrap();
        assert_eq!(meta.file_size, Some(100), "竞速应选中主源");
        assert!(
            !mirror_protocol.probe_ok.lock().await.is_empty(),
            "probe 后应记录可用源"
        );

        mirror_protocol.clear_selected().await;
        assert!(
            mirror_protocol.probe_ok.lock().await.is_empty(),
            "clear_selected 后应清空可用源"
        );
    }

    #[tokio::test]
    async fn test_download_fallback_to_mirror_when_primary_fails() {
        let failing_primary =
            Arc::new(MockProtocol::new().with_download_data(Err("primary failed".into())));

        let working_mirror = Arc::new(
            MockProtocol::new().with_download_data(Ok(Bytes::from_static(b"mirror data"))),
        );

        let mirror_protocol = MirrorProtocol::new(
            failing_primary,
            vec![("http://mirror.com/file".into(), working_mirror)],
        );

        let result = mirror_protocol
            .download_full("http://primary.com/file")
            .await;
        assert_eq!(
            result.unwrap(),
            Bytes::from_static(b"mirror data"),
            "主源失败时应回退到镜像源"
        );
    }

    #[tokio::test]
    async fn test_download_range_fallback_to_mirror_when_primary_fails() {
        let failing_primary =
            Arc::new(MockProtocol::new().with_download_data(Err("primary range failed".into())));

        let working_mirror = Arc::new(
            MockProtocol::new().with_download_data(Ok(Bytes::from_static(b"mirror range"))),
        );

        let mirror_protocol = MirrorProtocol::new(
            failing_primary,
            vec![("http://mirror.com/file".into(), working_mirror)],
        );

        let result = mirror_protocol
            .download_range("http://primary.com/file", 0, 99)
            .await;
        assert_eq!(
            result.unwrap(),
            Bytes::from_static(b"mirror range"),
            "主源 range 失败时应回退到镜像源"
        );
    }

    #[tokio::test]
    async fn test_download_range_stream_fallback_to_mirror_when_primary_fails() {
        let failing_primary =
            Arc::new(MockProtocol::new().with_download_data(Err("primary stream failed".into())));

        let working_mirror = Arc::new(
            MockProtocol::new().with_download_data(Ok(Bytes::from_static(b"mirror stream"))),
        );

        let mirror_protocol = MirrorProtocol::new(
            failing_primary,
            vec![("http://mirror.com/file".into(), working_mirror)],
        );

        let result = mirror_protocol
            .download_range_stream("http://primary.com/file", 0, 99)
            .await;
        assert!(result.is_ok(), "主源流式失败时应回退到镜像源");

        let mut stream = result.unwrap();
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(chunk, Bytes::from_static(b"mirror stream"));
    }

    /// 验证 probe 选中镜像后,后续 download 用选中源对应的 URL 而非主源 URL。
    ///
    /// 回归测试:曾因 selected 只存协议不存 URL,导致 probe 选中镜像后 download_range
    /// 仍传主源 URL,镜像协议拿着主源 URL 请求镜像服务器而失败(HF Race 模式失效)。
    #[tokio::test]
    async fn test_download_uses_selected_mirror_url_not_primary_url() {
        // 主源 probe 失败(模拟官方源国内探测不通),下载也失败
        let primary = Arc::new(
            MockProtocol::new()
                .with_probe_meta(Err("primary probe failed".into()))
                .with_download_data(Err("primary download failed".into())),
        );

        // 镜像 probe 成功,且只接受镜像 URL(expected_url),收到主源 URL 则失败
        let mirror = Arc::new(
            MockProtocol::new()
                .with_probe_meta(Ok(FileMetadata {
                    file_name: "model.bin".into(),
                    file_size: Some(100),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                    file_layout: None,
                }))
                .with_download_data(Ok(Bytes::from_static(b"from mirror")))
                .with_expected_url("http://mirror.com/file"),
        );

        let mirror_protocol =
            MirrorProtocol::new(primary, vec![("http://mirror.com/file".into(), mirror)]);

        // probe 竞速:仅镜像成功,选中镜像并记录镜像 URL
        let meta = mirror_protocol.probe("http://primary.com/file").await;
        assert!(meta.is_ok(), "probe 应选中镜像成功");

        // 下载:必须用镜像 URL 才能成功。若错误地用主源 URL,镜像会因 expected_url 不匹配而失败
        let result = mirror_protocol
            .download_range("http://primary.com/file", 0, 99)
            .await;
        assert!(
            result.is_ok(),
            "probe 选中镜像后下载应使用镜像 URL,实际错误: {:?}",
            result.err()
        );
        assert_eq!(result.unwrap(), Bytes::from_static(b"from mirror"));
    }

    /// 验证 probe 选中源下载失败时,回退到全源竞速(而非锁死在选中源)。
    ///
    /// 回归测试:Race 模式下 probe 阶段官方源可能"伪成功"(DNS 污染返回 200/302)被选中,
    /// 但实际下载失败。若 selected 命中后直接 return 错误,镜像永远不被尝试,
    /// 表现为"竞速仍只走官方"。修复:selected 失败后清空并落到全源竞速。
    #[tokio::test]
    async fn test_download_falls_back_to_race_when_selected_source_fails() {
        // 官方源 probe 成功(伪成功)但下载失败
        let primary = Arc::new(
            MockProtocol::new()
                .with_probe_meta(Ok(FileMetadata {
                    file_name: "model.bin".into(),
                    file_size: Some(100),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                    file_layout: None,
                }))
                .with_download_data(Err("primary download blocked".into())),
        );

        // 镜像 probe 成功,下载也成功(只接受镜像 URL)
        let mirror = Arc::new(
            MockProtocol::new()
                .with_probe_meta(Ok(FileMetadata {
                    file_name: "model.bin".into(),
                    file_size: Some(100),
                    content_type: None,
                    supports_range: true,
                    etag: None,
                    last_modified: None,
                    file_layout: None,
                }))
                .with_download_data(Ok(Bytes::from_static(b"from mirror")))
                .with_expected_url("http://mirror.com/file"),
        );

        let mirror_protocol =
            MirrorProtocol::new(primary, vec![("http://mirror.com/file".into(), mirror)]);

        // probe:官方先返回成功被选中(selected = 官方源)
        let meta = mirror_protocol.probe("http://primary.com/file").await;
        assert!(meta.is_ok(), "probe 应成功(官方伪成功)");

        // 下载:官方(选中源)失败后,必须回退到镜像竞速,而非直接报错
        let result = mirror_protocol
            .download_range("http://primary.com/file", 0, 99)
            .await;
        assert!(
            result.is_ok(),
            "选中源下载失败应回退全源竞速,实际错误: {:?}",
            result.err()
        );
        assert_eq!(result.unwrap(), Bytes::from_static(b"from mirror"));
    }

    // ===== P2: least-in-flight 多源并发调度测试 =====

    /// least-in-flight 核心:无 probe(无 selected)时,并发调用应让多源都参与
    /// (聚合带宽),而非主源 500ms 快径独占。快源多干但慢源不饿死。
    ///
    /// 当前 race_download 无 selected 时走"主源500ms+全源竞速":
    /// 主源(fast)500ms 内成功就独占,慢源不被调 → 多源未聚合 → 此测试应失败(RED)。
    #[tokio::test]
    async fn test_least_in_flight_multiple_sources_both_engaged() {
        // 快源 delay=1ms,慢源 delay=10ms;两者都健康
        let fast = Arc::new(MockProtocol::new().with_download_delay(Duration::from_millis(1)));
        let slow = Arc::new(MockProtocol::new().with_download_delay(Duration::from_millis(10)));

        let fast_counter = fast.select_counter.clone();
        let slow_counter = slow.select_counter.clone();

        // primary=fast, mirrors=[slow]。不调 probe(无 selected),直接并发 download。
        let mp = Arc::new(MirrorProtocol::new(
            fast.clone(),
            vec![("http://slow/file".into(), slow.clone())],
        ));

        // 并发 8 次 download_range_stream(模拟 8 个分片)
        let mut handles = tokio::task::JoinSet::new();
        for i in 0..8 {
            let mp = mp.clone();
            handles.spawn(async move {
                mp.download_range_stream("http://fast/file", i * 100, i * 100 + 99)
                    .await
                    .expect("下载失败")
            });
        }
        while handles.join_next().await.is_some() {}

        let fast_count = fast_counter.load(std::sync::atomic::Ordering::Relaxed);
        let slow_count = slow_counter.load(std::sync::atomic::Ordering::Relaxed);
        // least-in-flight 期望:两个源都参与(多源聚合带宽),快源多干
        assert!(
            slow_count > 0,
            "least-in-flight 应让慢源也参与(多源聚合),当前慢源被忽略: fast={fast_count}, slow={slow_count}"
        );
        assert!(
            fast_count >= slow_count,
            "快源应至少和慢源一样多被选: fast={fast_count}, slow={slow_count}"
        );
        // 总调用 = 8(每分片一个源拉,不浪费带宽)
        assert_eq!(
            fast_count + slow_count,
            8,
            "每分片应只一个源拉(不浪费带宽): fast={fast_count}, slow={slow_count}"
        );
    }

    /// least-in-flight:有 probe(有 selected)时,selected 不再独占,
    /// 多源仍并发参与(聚合带宽),快源多干。
    #[tokio::test]
    async fn test_least_in_flight_selected_not_exclusive() {
        let fast = Arc::new(MockProtocol::new().with_download_delay(Duration::from_millis(1)));
        let slow = Arc::new(
            MockProtocol::new()
                .with_probe_delay(Duration::from_millis(50))
                .with_download_delay(Duration::from_millis(10)),
        );

        let fast_counter = fast.select_counter.clone();
        let slow_counter = slow.select_counter.clone();

        let mp = Arc::new(MirrorProtocol::new(
            fast.clone(),
            vec![("http://slow/file".into(), slow.clone())],
        ));

        // probe 竞速:fast probe_delay=0 最快,被 selected
        let _ = mp.probe("http://fast/file").await;

        // 并发 8 次:least-in-flight 下 selected 不独占,慢源也应参与
        let mut handles = tokio::task::JoinSet::new();
        for i in 0..8 {
            let mp = mp.clone();
            handles.spawn(async move {
                mp.download_range_stream("http://fast/file", i * 100, i * 100 + 99)
                    .await
                    .expect("下载失败")
            });
        }
        while handles.join_next().await.is_some() {}

        let fast_count = fast_counter.load(std::sync::atomic::Ordering::Relaxed);
        let slow_count = slow_counter.load(std::sync::atomic::Ordering::Relaxed);
        // selected 不独占:慢源也应被选(多源聚合)
        assert!(
            slow_count > 0,
            "selected 不应独占,慢源也应参与: fast={fast_count}, slow={slow_count}"
        );
        assert_eq!(fast_count + slow_count, 8, "每分片一个源,不浪费带宽");
    }

    /// P3 质量回填:串行场景下,慢源(作为 primary/index 0)应被 quality 降权,
    /// 快源(mirror)质量高应多被选。
    ///
    /// 当前 least-in-flight 串行下总选 index 0(tie-break),若 slow 是 index 0
    /// 则 slow=8/fast=0(未感知质量)→ 此测试应失败(RED)。
    #[tokio::test]
    async fn test_quality_aware_slow_primary_demoted_in_serial() {
        // slow 作为 primary(index 0),fast 作为 mirror(index 1)
        let slow = Arc::new(MockProtocol::new().with_download_delay(Duration::from_millis(30)));
        let fast = Arc::new(MockProtocol::new().with_download_delay(Duration::from_millis(1)));

        let slow_counter = slow.select_counter.clone();
        let fast_counter = fast.select_counter.clone();

        let mp = Arc::new(MirrorProtocol::new(
            slow.clone(),
            vec![("http://fast/file".into(), fast.clone())],
        ));
        // probe 让两源都可用
        let _ = mp.probe("http://slow/file").await;

        // 串行下载 8 个分片(非并发,模拟低并发场景)
        // 必须消费流到 EOF,StatsStream 才会记录 success(更新 quality)
        for i in 0..8u64 {
            let stream = mp
                .download_range_stream("http://slow/file", i * 100, i * 100 + 99)
                .await
                .unwrap();
            // 消费流到 EOF,触发 StatsStream record_success
            use futures::StreamExt;
            let mut s = Box::pin(stream);
            while s.next().await.is_some() {}
        }

        let slow_count = slow_counter.load(std::sync::atomic::Ordering::Relaxed);
        let fast_count = fast_counter.load(std::sync::atomic::Ordering::Relaxed);
        // P3 quality:快源(fast)质量高应多被选,慢源(slow, index 0)应被降权
        assert!(
            fast_count > slow_count,
            "质量感知应让快源多干即使 slow 是 index 0: fast={fast_count}, slow={slow_count}"
        );
    }

    /// bench 计时:多源并发(least-in-flight) vs 单源串行,验证收益>10%
    ///
    /// 8 分片,2 源(快 5ms,慢 20ms)。
    /// - 单源串行(只用快源):≈ 8 × 5ms = 40ms
    /// - least-in-flight 多源并发:快源多干 + 慢源分担,理论 < 40ms
    ///
    /// **局限声明**:MockProto 延迟模拟"源速度差异",非真实网络带宽。
    /// 真实多源聚合带宽收益需联网 e2e 验证。此 bench 验证"机制有效"(并发加速),
    /// AGENTS.md 要求>10% 以绝对计时为准(Windows criterion 相对变化不可信)。
    #[tokio::test(flavor = "multi_thread")]
    async fn bench_multi_source_vs_single_source_throughput() {
        let frag_count = 8usize;
        let fast_delay = Duration::from_millis(5);
        let slow_delay = Duration::from_millis(20);

        // 基线:单源(快源)串行下载 8 分片
        let single = Arc::new(MockProtocol::new().with_download_delay(fast_delay));
        let single_start = std::time::Instant::now();
        for i in 0..frag_count {
            let stream = single
                .download_range_stream("http://fast/file", i as u64 * 100, i as u64 * 100 + 99)
                .await
                .unwrap();
            // 消费流到 EOF(触发 delay,模拟真实下载)
            use futures::StreamExt;
            let mut s = Box::pin(stream);
            while s.next().await.is_some() {}
        }
        let single_elapsed = single_start.elapsed();

        // 多源并发:least-in-flight(快源 + 慢源),8 分片并发
        let fast = Arc::new(MockProtocol::new().with_download_delay(fast_delay));
        let slow = Arc::new(MockProtocol::new().with_download_delay(slow_delay));
        let mp = Arc::new(MirrorProtocol::new(
            fast.clone(),
            vec![("http://slow/file".into(), slow.clone())],
        ));
        // probe 让两源都进入 probe_ok
        let _ = mp.probe("http://fast/file").await;

        let multi_start = std::time::Instant::now();
        let mut handles = tokio::task::JoinSet::new();
        for i in 0..frag_count {
            let mp = mp.clone();
            handles.spawn(async move {
                let stream = mp
                    .download_range_stream("http://fast/file", i as u64 * 100, i as u64 * 100 + 99)
                    .await
                    .unwrap();
                // 消费流到 EOF(触发 delay)
                use futures::StreamExt;
                let mut s = Box::pin(stream);
                while s.next().await.is_some() {}
            });
        }
        while handles.join_next().await.is_some() {}
        let multi_elapsed = multi_start.elapsed();

        let speedup = single_elapsed.as_secs_f64() / multi_elapsed.as_secs_f64();
        let improvement = (speedup - 1.0) * 100.0;
        eprintln!(
            "单源串行: {:?}, 多源并发(least-in-flight): {:?}, 加速 {:.1}x, 收益 +{:.0}%",
            single_elapsed, multi_elapsed, speedup, improvement
        );
        // AGENTS.md:引入并发复杂度的优化需证明收益>10%
        assert!(
            improvement > 10.0,
            "多源并发收益 {improvement:.0}% 未达 10% 门禁(单源 {single_elapsed:?} vs 多源 {multi_elapsed:?})"
        );
    }
}
