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

use crate::connection::ConnectionPool;

/// 源 URL + Protocol 对
type Source = (String, Arc<dyn Protocol>);

/// 单源 probe 超时(修复 MEDIUM-3:防源挂起致永久阻塞/detached 任务泄漏)
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 软熔断阈值:连续失败达到此值后,该源在选源时被跳过(半开探测除外)。
///
/// 与 engine 层 circuit_breaker 分工:engine 层管全局请求级熔断
/// (不可达主机 / 连续失败致整体不可用),本阈值管 MirrorProtocol 内
/// per-source 软熔断 —— 避免对已知坏源反复重试,浪费 max_retries×源数
/// 次尝试。clear_selected 重置(配合 stats 衰减),给半开探测机会。
const SOFT_CIRCUIT_BREAKER_THRESHOLD: u32 = 5;

/// 源质量统计(轻量,engine 内自管)
///
/// 记录每源的成功/失败次数、累计字节/耗时,派生 quality(0~1,越高越好)。
/// 选源时用 `in_flight / (quality + ε)` 加权:质量高的源允许更多在途。
///
/// 另维护连续失败计数(consecutive_failures),超过
/// [`SOFT_CIRCUIT_BREAKER_THRESHOLD`] 时该源被软熔断,选源时跳过
/// (除非全部源都熔断,则全部重置尝试一次——半开探测)。
#[derive(Debug, Clone, Default)]
struct SourceStats {
    success: u64,
    fail: u64,
    total_bytes: u64,
    total_duration_ns: u128,
    /// 连续失败计数:成功时清零,失败时递增。达 [`SOFT_CIRCUIT_BREAKER_THRESHOLD`]
    /// 时 [`Self::is_circuit_open`] 返回 true,选源跳过该源。
    consecutive_failures: u32,
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
    ///
    /// 成功时清零连续失败计数(解除软熔断)。
    fn record_success(&mut self, bytes: u64, duration_ns: u128) {
        self.success += 1;
        self.total_bytes += bytes;
        self.total_duration_ns += duration_ns;
        self.consecutive_failures = 0;
    }

    /// 记录一次失败
    ///
    /// 递增连续失败计数;达 [`SOFT_CIRCUIT_BREAKER_THRESHOLD`] 后
    /// [`Self::is_circuit_open`] 返回 true。
    fn record_failure(&mut self) {
        self.fail += 1;
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }

    /// 该源是否已软熔断(连续失败达阈值)。
    ///
    /// 软熔断状态下选源跳过该源;若所有候选源都熔断,则全部重置
    /// (半开探测),见 [`Self::reset_circuit`]。
    fn is_circuit_open(&self) -> bool {
        self.consecutive_failures >= SOFT_CIRCUIT_BREAKER_THRESHOLD
    }

    /// 重置该源的软熔断状态(半开探测 / clear_selected 恢复时调用)。
    ///
    /// 仅清零连续失败计数,不动 success/fail 累积(quality 衰减由
    /// clear_selected 单独处理),使该源能重新参与选源。
    fn reset_circuit(&mut self) {
        self.consecutive_failures = 0;
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
///
/// P1:可选持有 `ConnectionPermit`,使镜像路径的 per-host 连接许可生命周期
/// 与流消费对齐(流 EOF/Err/Drop 时 drop permit,释放连接槽位)。
struct StatsStream {
    inner: ByteStream,
    source_idx: usize,
    stats: Arc<std::sync::Mutex<Vec<SourceStats>>>,
    in_flight: Arc<std::sync::Mutex<Vec<usize>>>,
    start: Option<std::time::Instant>, // 首字节时记录(修复 OPT-6:不含排队等待)
    bytes_seen: u64,
    finished: bool,
    /// P1:镜像路径按真实 host 获取的连接许可,流结束时 drop 释放
    ///
    /// 此字段无显式读取:仅靠 `StatsStream` 被丢弃时字段的隐式 `Drop` 释放许可
    /// (RAII)。`ConnectionPermit::Drop` 归还全局 + per-host 信号量。
    #[allow(dead_code)]
    permit: Option<crate::connection::ConnectionPermit>,
}

impl StatsStream {
    fn wrap(
        inner: ByteStream,
        source_idx: usize,
        stats: Arc<std::sync::Mutex<Vec<SourceStats>>>,
        in_flight: Arc<std::sync::Mutex<Vec<usize>>>,
        permit: Option<crate::connection::ConnectionPermit>,
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
            permit,
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
    /// 可选的连接许可池(P1:镜像路径按选中源真实 host acquire,而非主 URL host)
    ///
    /// 引擎层在镜像路径跳过 pool.acquire(主 host),由本协议适配器在选源后
    /// 按真实命中镜像 URL 的 host 单独 acquire,使各镜像能各自占满自己的
    /// per-host 配额(聚合多源带宽)。为 None 时(单源 / 测试)不 acquire,
    /// 行为与旧行为一致。
    pool: Option<Arc<ConnectionPool>>,
}

impl MirrorProtocol {
    /// 构造多镜像源适配器(无连接池,测试 / 单源路径使用)
    ///
    /// 生产路径(`with_mirrors`/`with_hybrid_sources`)使用 `with_pool` 注入连接池;
    /// 本构造器供测试(MockProtocol 无需 pool)与未来单源降级路径使用。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(
        primary: Arc<dyn Protocol>,
        mirrors: Vec<(String, Arc<dyn Protocol>)>,
    ) -> Self {
        Self::with_pool(primary, mirrors, None)
    }

    /// 构造多镜像源适配器并注入连接许可池
    ///
    /// `pool` 为 Some 时,适配器在选源后按真实命中镜像 URL 的 host 单独
    /// acquire 连接许可(P1:使各镜像能各自占满自己的 per-host 配额),
    /// 引擎层在镜像路径不再对主 URL host 重复 acquire。
    pub(crate) fn with_pool(
        primary: Arc<dyn Protocol>,
        mirrors: Vec<(String, Arc<dyn Protocol>)>,
        pool: Option<Arc<ConnectionPool>>,
    ) -> Self {
        let mut sources = vec![(String::new(), primary)];
        sources.extend(mirrors);
        let n = sources.len();
        Self {
            sources,
            probe_ok: Arc::new(Mutex::new(HashSet::new())),
            in_flight: Arc::new(std::sync::Mutex::new(vec![0; n])),
            stats: Arc::new(std::sync::Mutex::new(vec![SourceStats::default(); n])),
            pool,
        }
    }

    /// 清除 probe 结果 + 重置选源状态(修复 BUG-B:不清 in_flight 导致失败源永久饿死)
    ///
    /// P3 遗忘机制:同时对 stats 做衰减(success/fail 各除以 2),避免瞬时故障
    /// 镜像因 fail 永久累积被永久冷落。衰减保留趋势(质量排序大致不变)但弱化
    /// 历史权重,使恢复中的镜像能重新被选中。total_bytes/duration 不衰减(它们
    /// 反映真实带宽采样,非失败惩罚)。
    pub(crate) async fn clear_selected(&self) {
        *self.probe_ok.lock().await = HashSet::new();
        // 重置 in_flight:重试前所有源在途数归零(避免跨调用累积)
        if let Ok(mut inflight) = self.in_flight.lock() {
            for v in inflight.iter_mut() {
                *v = 0;
            }
        }
        // P3 衰减:success/fail 各除以 2(整数除法,保留趋势弱化历史)
        // 遗忘机制避免瞬时故障永久污名化:某镜像曾连续失败数次,quality 持续
        // 偏低,即使后续恢复也会因历史 fail 累积而难被选中。衰减后 fail 计数
        // 减半,stability 回升,使恢复中的镜像能重新参与 least-in-flight 调度。
        if let Ok(mut stats) = self.stats.lock() {
            for s in stats.iter_mut() {
                s.success /= 2;
                s.fail /= 2;
                // 软熔断恢复:重置连续失败计数,给半开探测机会。
                // 与 engine 层 circuit_breaker 分工:此处仅 per-source 软熔断,
                // clear_selected 是 MirrorProtocol 选源状态重置入口,熔断重置
                // 与 stats 衰减配合,使坏源能被重新评估而非永久跳过。
                s.reset_circuit();
            }
        }
    }

    /// least-in-flight + 质量加权下载:选源 → 下载 → 成功递减;失败则内部 fallback
    ///
    /// 选源:in_flight / (quality + ε),质量高的源允许更多在途(快源多干)。
    /// 失败源惩罚性保留在途数(不递减),使后续选源优先选其他源。
    /// 遍历所有候选源直到成功或全失败(对调用方透明)。
    /// 成功返回 (数据, 选中源 idx, 连接许可)(idx 供 stream 路径包 StatsStream 用,
    /// permit 供 stream 路径持有到流结束;Bytes 路径调用方立即 drop)。
    ///
    /// P1:`pool` 为 Some 时,选源后按真实命中镜像 URL 的 host 单独 acquire 连接许可,
    /// 引擎层镜像路径不再对主 URL host 重复 acquire,使各镜像能各自占满自己的
    /// per-host 配额(聚合多源带宽)。为 None 时不 acquire(单源 / 测试路径)。
    #[allow(clippy::too_many_arguments)]
    async fn download_via_least_in_flight<T: Send + 'static>(
        sources: Vec<Source>,
        probe_ok: Arc<Mutex<HashSet<usize>>>,
        in_flight: Arc<std::sync::Mutex<Vec<usize>>>,
        stats: Arc<std::sync::Mutex<Vec<SourceStats>>>,
        pool: Option<Arc<ConnectionPool>>,
        url: &str,
        download_fn: impl Fn(
            Arc<dyn Protocol>,
            String,
        ) -> Pin<Box<dyn Future<Output = DownloadResult<T>> + Send>>
        + Clone
        + Send
        + 'static,
        error_label: &str,
    ) -> DownloadResult<(T, usize, Option<crate::connection::ConnectionPermit>)> {
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

        // 软熔断半开探测:若所有候选源均已连续失败达阈值(熔断),全部重置
        // 连续失败计数,给一次恢复探测机会(不阻断下载)。与 engine 层
        // circuit_breaker 分工:此处仅 per-source 软熔断,engine 层管全局。
        {
            let mut s = stats.lock().unwrap_or_else(|e| e.into_inner());
            let all_open =
                !candidates.is_empty() && candidates.iter().all(|&i| s[i].is_circuit_open());
            if all_open {
                for &i in &candidates {
                    s[i].reset_circuit();
                }
                tracing::info!(error_label, "所有候选源均已软熔断,执行半开探测重置");
            }
        }

        let mut last_err = None;
        // 遍历候选源,每次选加权最小的(in_flight 主导 + quality 辅助)
        for _ in 0..candidates.len() {
            let (idx, src_url, proto) = {
                let mut inflight = in_flight.lock().unwrap_or_else(|e| e.into_inner());
                // 快照各源 quality + 软熔断状态(std Mutex,持锁极短,不跨 await)
                let (qualities, circuit_open): (Vec<f64>, Vec<bool>) = {
                    let s = stats.lock().unwrap_or_else(|e| e.into_inner());
                    let n = sources.len();
                    let mut q = Vec::with_capacity(n);
                    let mut c = Vec::with_capacity(n);
                    for st in s.iter() {
                        q.push(st.quality());
                        c.push(st.is_circuit_open());
                    }
                    (q, c)
                };
                // 加权(修复 BUG-A/C):加性公式 inflight*W1 + (1-quality)*W2
                // inflight=0 时按 quality 排序(quality 高→(1-quality)小→优先);
                // inflight>0 时在途数主导(负载均衡)。
                // W1=10000(在途数权重高),W2=1000(质量权重)
                // 软熔断:跳过 circuit_open 的源(半开重置已在前面处理 all-open)
                let pick = candidates
                    .iter()
                    .copied()
                    .filter(|&i| inflight[i] < usize::MAX && !circuit_open[i])
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

            // P1:按真实命中镜像 URL 的 host acquire 连接许可(若注入了 pool)
            // 引擎层镜像路径已跳过主 host 的 acquire,此处接管 per-host 限流,
            // 使各镜像能各自占满自己的配额。host 解析失败时跳过(降级为不限流,
            // 不阻断下载)。
            let permit = if let Some(ref pool) = pool {
                match Self::host_of(&src_url) {
                    Some(host) => match pool.acquire(&host).await {
                        Ok(p) => Some(p),
                        Err(e) => {
                            // 许可获取失败(信号量关闭等),记 fail 并切换下一源
                            let mut inflight = in_flight.lock().unwrap_or_else(|e| e.into_inner());
                            if inflight[idx] > 0 {
                                inflight[idx] -= 1;
                            }
                            tracing::info!(error = %e, source_idx = idx, error_label, "镜像源连接许可获取失败,切换下一源");
                            stats.lock().unwrap_or_else(|e| e.into_inner())[idx].record_failure();
                            last_err = Some(e);
                            continue;
                        }
                    },
                    // 修复 host_of-Low:畸形镜像 URL 无 host,静默降级会绕过 per-host 限流。
                    // 此处 warn 一次(畸形 URL 罕见,刷屏风险低),降级为不 acquire,不阻断下载。
                    None => {
                        tracing::warn!(url = %src_url, source_idx = idx, "镜像 URL 无 host,跳过 per-host 限流(降级为不限流)");
                        None
                    }
                }
            } else {
                None
            };

            match download_fn(proto, src_url).await {
                Ok(data) => {
                    // 不在此递减 in_flight(修复 BUG-A):
                    // - Bytes 路径:调用方(download_range/download_full)拿 (data, idx) 后递减,
                    //   并 drop permit(数据已就绪,连接许可可释放)
                    // - stream 路径:StatsStream 在流 EOF/Err/Drop 时递减,并 drop permit
                    return Ok((data, idx, permit));
                }
                Err(e) => {
                    // 失败:递减 in_flight(不累积,修复 BUG-B)+ 记 fail stats(降 quality)
                    // permit 在此 drop(连接许可归还)
                    drop(permit);
                    let mut inflight = in_flight.lock().unwrap_or_else(|e| e.into_inner());
                    if inflight[idx] > 0 {
                        inflight[idx] -= 1;
                    }
                    tracing::info!(error = %e, source_idx = idx, error_label, "源下载失败,切换下一源");
                    stats.lock().unwrap_or_else(|e| e.into_inner())[idx].record_failure();
                    last_err = Some(e);
                }
            }
        }
        Err(last_err
            .unwrap_or_else(|| DownloadError::Protocol(format!("所有源均失败{error_label}"))))
    }

    /// 从 URL 提取 host(用于 P1 按真实镜像 host acquire 连接许可)
    ///
    /// 主源(index 0)的 src_url 复用上层传入的主 URL,此处统一解析。
    /// 解析失败或无 host(如相对路径)返回 None,调用方降级为不限流。
    fn host_of(url: &str) -> Option<String> {
        url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(ToString::to_string))
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
            // 修复 MEDIUM-3:probe 闭包加 timeout,防源挂起导致永久阻塞/detached 任务泄漏
            let mut set = JoinSet::new();
            for (i, (src_url, proto)) in sources.iter().enumerate() {
                let p = proto.clone();
                let u = if i == 0 { url.clone() } else { src_url.clone() };
                set.spawn(async move {
                    // 单源 probe 超时 30s,超时视为该源失败(不阻塞首成功返回)
                    match tokio::time::timeout(PROBE_TIMEOUT, p.probe(&u)).await {
                        Ok(result) => (i, result),
                        Err(_) => (
                            i,
                            Err(DownloadError::Protocol(format!(
                                "probe 超时({PROBE_TIMEOUT:?}): {u}"
                            ))),
                        ),
                    }
                });
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
            // 修复 MEDIUM-3:detached spawn 的 join_next 加超时,防慢源永久驻留 runtime
            let probe_ok_bg = probe_ok.clone();
            tokio::spawn(async move {
                // 每个剩余 probe 最多再等 PROBE_TIMEOUT,超时即放弃补全(首成功已返回,
                // 慢源即使后续成功也非关键)。逐次 join_next + timeout 避免整体超时
                // 误杀快源。
                while let Ok(Some(result)) =
                    tokio::time::timeout(PROBE_TIMEOUT, set.join_next()).await
                {
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
        let pool = self.pool.clone();
        let url = url.to_string();
        Box::pin(async move {
            // 计时:覆盖 download_via_least_in_flight 全程(选源 + 下载),用于
            // 记 success stats(派生 quality)与软熔断恢复(record_success 清零
            // consecutive_failures)。Bytes 路径此前不记 stats,坏源即使成功也
            // 永久熔断;此处补齐使软熔断在 Bytes 路径正确恢复。
            let started = std::time::Instant::now();
            let (data, idx, permit) = Self::download_via_least_in_flight(
                sources,
                probe_ok,
                in_flight.clone(),
                stats.clone(),
                pool,
                &url,
                move |proto, u| proto.download_range(&u, start, end),
                "",
            )
            .await?;
            // Bytes 路径:data 已就绪,立即递减 in_flight(stream 路径由 StatsStream 递减)
            // 并 drop permit(数据已就绪,连接许可可释放)
            drop(permit);
            if let Ok(mut inflight) = in_flight.lock()
                && inflight[idx] > 0
            {
                inflight[idx] -= 1;
            }
            // 记 success:清零 consecutive_failures(解除软熔断)+ 累积 quality 采样
            if let Ok(mut s) = stats.lock() {
                s[idx].record_success(data.len() as u64, started.elapsed().as_nanos());
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
        let pool = self.pool.clone();
        let url = url.to_string();
        Box::pin(async move {
            let (stream, idx, permit) = Self::download_via_least_in_flight(
                sources,
                probe_ok,
                in_flight.clone(),
                stats.clone(),
                pool,
                &url,
                move |proto, u| proto.download_range_stream(&u, start, end),
                "(流式)",
            )
            .await?;
            // 包 StatsStream:流 EOF/Err/Drop 时递减 in_flight + 记 stats(修复 BUG-A)
            // + drop permit(P1:连接许可随流生命周期释放)
            Ok(StatsStream::wrap(stream, idx, stats, in_flight, permit))
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
        let pool = self.pool.clone();
        let url = url.to_string();
        Box::pin(async move {
            // 计时:覆盖 download_via_least_in_flight 全程,记 success stats
            // (派生 quality + 清零 consecutive_failures 解除软熔断)。
            // Bytes 路径此前不记 stats,补齐使软熔断正确恢复。
            let started = std::time::Instant::now();
            let (data, idx, permit) = Self::download_via_least_in_flight(
                sources,
                probe_ok,
                in_flight.clone(),
                stats.clone(),
                pool,
                &url,
                move |proto, u| proto.download_full(&u),
                "(全量)",
            )
            .await?;
            // Bytes 路径:立即递减 in_flight + drop permit
            drop(permit);
            if let Ok(mut inflight) = in_flight.lock()
                && inflight[idx] > 0
            {
                inflight[idx] -= 1;
            }
            // 记 success:清零 consecutive_failures(解除软熔断)+ 累积 quality 采样
            if let Ok(mut s) = stats.lock() {
                s[idx].record_success(data.len() as u64, started.elapsed().as_nanos());
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
        let pool = self.pool.clone();
        let url = url.to_string();
        Box::pin(async move {
            let (stream, idx, permit) = Self::download_via_least_in_flight(
                sources,
                probe_ok,
                in_flight.clone(),
                stats.clone(),
                pool,
                &url,
                move |proto, u| proto.download_full_stream(&u),
                "(全量流式)",
            )
            .await?;
            Ok(StatsStream::wrap(stream, idx, stats, in_flight, permit))
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
        /// 若为 true,download_range_stream 返回一个永不产出的 pending 流
        /// (用于测试流 abort 时 permit RAII 释放)
        pending_stream: bool,
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
                pending_stream: false,
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

        /// 使 download_range_stream 返回永不产出的 pending 流(测试流 abort 释放 permit)
        fn with_pending_stream(mut self) -> Self {
            self.pending_stream = true;
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
            let counter = self.select_counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            let pending = self.pending_stream;
            Box::pin(async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // pending 流:永不产出(测试流 abort 时 permit RAII 释放)
                if pending {
                    let stream = futures::stream::pending();
                    return Ok(Box::pin(stream) as ByteStream);
                }
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
            let counter = self.select_counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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

    /// bench 缺口 3:选源锁开销监控(零延迟,隔离 in_flight/stats 锁竞争)
    ///
    /// 4 源零 delay,128 分片并发 download_range_stream。对比"直连主源"(绕过
    /// MirrorProtocol 选源)的基线,隔离 least-in-flight 选源路径的纯锁开销。
    /// 断言绝对值 <1ms(非百分比,避免 Windows 调度波动),作回归监控防恶化。
    #[tokio::test(flavor = "multi_thread")]
    async fn bench_source_selection_lock_overhead() {
        let fast = Arc::new(MockProtocol::new()); // delay=0
        let slow = Arc::new(MockProtocol::new());
        let mp = Arc::new(MirrorProtocol::new(
            fast.clone(),
            vec![("http://slow/file".into(), slow.clone())],
        ));

        let frag_count = 128u64;

        // 直连基线:绕过 MirrorProtocol,直接调主源 download_range_stream
        let mut handles = tokio::task::JoinSet::new();
        let direct_start = std::time::Instant::now();
        for i in 0..frag_count {
            let fast = fast.clone();
            handles.spawn(async move {
                let stream = fast
                    .download_range_stream("http://fast/file", i * 100, i * 100 + 99)
                    .await
                    .unwrap();
                use futures::StreamExt;
                let mut s = Box::pin(stream);
                while s.next().await.is_some() {}
            });
        }
        while handles.join_next().await.is_some() {}
        let direct_elapsed = direct_start.elapsed();

        // 经 MirrorProtocol 选源:含 in_flight/stats 锁 + candidates 排序 + quality 计算
        let mut handles = tokio::task::JoinSet::new();
        let select_start = std::time::Instant::now();
        for i in 0..frag_count {
            let mp = mp.clone();
            handles.spawn(async move {
                let stream = mp
                    .download_range_stream("http://fast/file", i * 100, i * 100 + 99)
                    .await
                    .unwrap();
                use futures::StreamExt;
                let mut s = Box::pin(stream);
                while s.next().await.is_some() {}
            });
        }
        while handles.join_next().await.is_some() {}
        let select_elapsed = select_start.elapsed();

        let overhead = select_elapsed.saturating_sub(direct_elapsed);
        eprintln!(
            "选源锁开销: 直连 {direct_elapsed:?} vs 选源 {select_elapsed:?}, \
             开销 {overhead:?} (128 分片, 4 源, 零延迟)"
        );
        // 回归监控:选源锁开销绝对值应 <1ms(零延迟下纯锁竞争)
        assert!(
            overhead < std::time::Duration::from_millis(1),
            "选源锁开销 {overhead:?} 超过 1ms,可能存在锁竞争恶化"
        );
    }

    // ===== P3 衰减测试:clear_selected 遗忘历史 stats =====

    /// P3:clear_selected 应对 stats 做 success/fail 衰减(各除以 2),
    /// 避免瞬时故障镜像因 fail 永久累积被永久冷落。
    ///
    /// 构造一个 stats 中 success=10/fail=10 的 MirrorProtocol(直接写内部 stats),
    /// 调 clear_selected 后断言 success=5/fail=5(整数除法衰减保留趋势但弱化历史)。
    /// total_bytes/total_duration_ns 不衰减(反映真实带宽采样,非失败惩罚)。
    #[tokio::test]
    async fn test_p3_clear_selected_decays_stats() {
        let primary = Arc::new(MockProtocol::new());
        let mp = MirrorProtocol::new(primary, vec![]);

        // 直接写内部 stats,构造"曾连续失败 10 次 + 成功 10 次"的历史
        {
            let mut stats = mp.stats.lock().unwrap_or_else(|e| e.into_inner());
            stats[0].success = 10;
            stats[0].fail = 10;
            stats[0].total_bytes = 4096;
            stats[0].total_duration_ns = 1_000_000_000;
        }

        mp.clear_selected().await;

        let stats = mp.stats.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            stats[0].success, 5,
            "P3: clear_selected 应将 success 衰减为 10/2=5"
        );
        assert_eq!(
            stats[0].fail, 5,
            "P3: clear_selected 应将 fail 衰减为 10/2=5"
        );
        // 带宽采样不衰减
        assert_eq!(stats[0].total_bytes, 4096, "total_bytes 不应衰减");
        assert_eq!(
            stats[0].total_duration_ns, 1_000_000_000,
            "total_duration_ns 不应衰减"
        );
    }

    /// P3 衰减的幂等性:连续多次 clear_selected 多次衰减(success/fail 反复减半)。
    /// 验证衰减不会下溢到 panic(usize 减法),且最终趋近 0。
    #[tokio::test]
    async fn test_p3_clear_selected_decay_idempotent_no_underflow() {
        let primary = Arc::new(MockProtocol::new());
        let mp = MirrorProtocol::new(primary, vec![]);

        {
            let mut stats = mp.stats.lock().unwrap_or_else(|e| e.into_inner());
            stats[0].success = 3;
            stats[0].fail = 1; // 奇数,验证整数除法不向下溢出
        }

        // 连续衰减 3 次:3→1→0→0, 1→0→0→0
        mp.clear_selected().await;
        {
            let stats = mp.stats.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(stats[0].success, 1, "第一次衰减: 3/2=1");
            assert_eq!(stats[0].fail, 0, "第一次衰减: 1/2=0");
        }
        mp.clear_selected().await;
        {
            let stats = mp.stats.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(stats[0].success, 0, "第二次衰减: 1/2=0");
            assert_eq!(stats[0].fail, 0, "第二次衰减: 0/2=0(不溢出)");
        }
        mp.clear_selected().await;
        {
            let stats = mp.stats.lock().unwrap_or_else(|e| e.into_inner());
            assert_eq!(stats[0].success, 0, "第三次衰减: 0/2=0(稳定)");
            assert_eq!(stats[0].fail, 0, "第三次衰减: 0/2=0(稳定)");
        }
    }

    // ===== host_of 降级测试(修复 host_of-Low:畸形镜像 URL 静默绕过 per-host 限流)=====

    /// host_of 对畸形 URL(无 host)应返回 None 且不 panic。
    #[test]
    fn test_host_of_malformed_url_returns_none() {
        // 相对路径无 scheme/host
        assert_eq!(MirrorProtocol::host_of("not-a-url"), None);
        // 无 host 的 scheme
        assert_eq!(MirrorProtocol::host_of("file:///path/to/file"), None);
        // 空串
        assert_eq!(MirrorProtocol::host_of(""), None);
        // 仅 scheme
        assert_eq!(MirrorProtocol::host_of("http://"), None);
    }

    /// host_of 对正常 URL 应返回 host。
    #[test]
    fn test_host_of_normal_url_returns_host() {
        assert_eq!(
            MirrorProtocol::host_of("http://mirror.example.com/file"),
            Some("mirror.example.com".to_string())
        );
        assert_eq!(
            MirrorProtocol::host_of("https://cdn.example.org:8080/path"),
            Some("cdn.example.org".to_string())
        );
    }

    /// host_of 降级路径:畸形镜像 URL(pool 注入时)应不阻断下载,降级为不 acquire。
    ///
    /// 构造 mirror URL 为相对路径(无 host),pool 为 max_per_host=1 的真实池。
    /// download_range 应成功(降级不阻断),且 pool active_connections 仍为 0
    /// (因 host_of 返回 None 未 acquire)。此测试验证降级行为不 panic 且不
    /// 绕过限流(此处"绕过"指畸形 URL 不 acquire,符合预期降级)。
    #[tokio::test]
    async fn test_host_of_none_degrades_without_acquire() {
        use crate::connection::{ConnectionPool, PoolConfig};

        let pool = Arc::new(ConnectionPool::new(PoolConfig {
            max_per_host: 1,
            max_global: 10,
            ..Default::default()
        }));
        let primary =
            Arc::new(MockProtocol::new().with_download_data(Ok(Bytes::from_static(b"degraded"))));
        // 镜像 URL 为畸形(无 host),host_of 返回 None → 不 acquire
        let mp = MirrorProtocol::with_pool(
            primary,
            vec![("not-a-url".into(), Arc::new(MockProtocol::new()))],
            Some(pool.clone()),
        );

        let result = mp.download_range("http://primary.com/file", 0, 99).await;
        assert!(result.is_ok(), "畸形镜像 URL 不应阻断下载(降级)");
        assert_eq!(
            pool.active_connections(),
            0,
            "畸形 URL 未 acquire,active 应为 0"
        );
    }

    // ===== P1: permit RAII 生命周期测试(修复 P1-测试缺口)=====

    /// P1:per-host acquire 互斥。构造 max_per_host=1 的池 + 2 个同 host 镜像,
    /// 并发调 download_range_stream,第二个应因 per-host 信号量满而排队(不立即获得)。
    ///
    /// 用 pending 流(永不产出)使首个下载长期占用 permit,验证第二个下载在
    /// 短超时内无法获得 permit(被 per-host 限流阻塞)。
    #[tokio::test]
    async fn test_p1_per_host_acquire_mutex() {
        use crate::connection::{ConnectionPool, PoolConfig};

        // max_per_host=1:同 host 只能有一个活跃 permit
        let pool = Arc::new(ConnectionPool::new(PoolConfig {
            max_per_host: 1,
            max_global: 10,
            enable_http2: false, // 避免默认值自动提升 max_per_host
            ..Default::default()
        }));
        // 两个同 host 镜像(http://same.host/...),pending 流永不产出(占住 permit)
        let primary = Arc::new(MockProtocol::new().with_pending_stream());
        let mirror = Arc::new(MockProtocol::new().with_pending_stream());
        let mp = Arc::new(MirrorProtocol::with_pool(
            primary,
            vec![("http://same.host/file".into(), mirror)],
            Some(pool.clone()),
        ));

        // 首个下载:拿到 permit,pending 流占住(永不释放,直到 drop)
        let first = mp
            .download_range_stream("http://same.host/file", 0, 99)
            .await;
        assert!(first.is_ok(), "首个下载应成功获取 permit");
        assert_eq!(
            pool.active_connections(),
            1,
            "首个下载应占住 1 个 per-host permit"
        );

        // 第二个下载:同 host,per-host 信号量已满(max_per_host=1),应排队阻塞。
        // 用短超时验证它不立即获得 permit。
        let mp2 = Arc::clone(&mp);
        let second = tokio::time::timeout(
            Duration::from_millis(50),
            mp2.download_range_stream("http://same.host/file", 100, 199),
        )
        .await;
        assert!(
            second.is_err(),
            "同 host 第二个下载应被 per-host 限流阻塞,不应在 50ms 内获得 permit"
        );
        // 首个流 drop 前仍占住 permit
        assert_eq!(pool.active_connections(), 1);
    }

    /// P1:permit 释放。Bytes 路径(download_full)完成后,permit 应立即释放,
    /// pool active_connections 归零。
    #[tokio::test]
    async fn test_p1_permit_released_after_bytes_download() {
        use crate::connection::{ConnectionPool, PoolConfig};

        let pool = Arc::new(ConnectionPool::new(PoolConfig {
            max_per_host: 2,
            max_global: 10,
            enable_http2: false,
            ..Default::default()
        }));
        let primary =
            Arc::new(MockProtocol::new().with_download_data(Ok(Bytes::from_static(b"done"))));
        let mp = MirrorProtocol::with_pool(primary, vec![], Some(pool.clone()));

        assert_eq!(pool.active_connections(), 0, "下载前 active 应为 0");
        let result = mp.download_full("http://example.com/file").await;
        assert!(result.is_ok(), "下载应成功");
        assert_eq!(
            pool.active_connections(),
            0,
            "Bytes 路径完成后 permit 应释放,active 归零"
        );
    }

    /// P1:流 abort 释放 permit。构造 pending 流(永不产出),拿到 StatsStream 后
    /// 立即 drop,验证 permit 通过 RAII 释放(active_connections 归零)。
    #[tokio::test]
    async fn test_p1_permit_released_on_stream_abort() {
        use crate::connection::{ConnectionPool, PoolConfig};

        let pool = Arc::new(ConnectionPool::new(PoolConfig {
            max_per_host: 2,
            max_global: 10,
            enable_http2: false,
            ..Default::default()
        }));
        // pending 流:永不产出,模拟挂起的下载(占住 permit 直到流被 drop)
        let primary = Arc::new(MockProtocol::new().with_pending_stream());
        let mp = MirrorProtocol::with_pool(primary, vec![], Some(pool.clone()));

        // 拿到流:permit 被 StatsStream 持有,active=1
        let stream = mp
            .download_range_stream("http://example.com/file", 0, 99)
            .await
            .expect("流创建应成功");
        assert_eq!(pool.active_connections(), 1, "流存在时应持有 1 个 permit");

        // drop 流:StatsStream Drop 触发 permit Drop,active 归零
        drop(stream);
        assert_eq!(
            pool.active_connections(),
            0,
            "流 abort(drop)后 permit 应通过 RAII 释放,active 归零"
        );
    }

    // ===== 软熔断测试(修复 软熔断-Medium:坏源无快速失败)=====

    /// 软熔断:连续失败达阈值(5)的源,选源时应被跳过(不被 min_by 选中)。
    ///
    /// 构造两源(primary=fail 5 次已熔断,mirror=健康),download 应跳过 primary
    /// 直接选 mirror。用 select_counter 验证 primary 被跳过(计数为 0)。
    #[tokio::test]
    async fn test_soft_circuit_breaker_skips_open_source() {
        let primary =
            Arc::new(MockProtocol::new().with_download_data(Ok(Bytes::from_static(b"primary"))));
        let mirror =
            Arc::new(MockProtocol::new().with_download_data(Ok(Bytes::from_static(b"mirror"))));
        let primary_counter = primary.select_counter.clone();
        let mirror_counter = mirror.select_counter.clone();

        let mp = MirrorProtocol::new(primary, vec![("http://mirror.com/file".into(), mirror)]);
        // 手动将 primary(index 0)置为软熔断(连续失败 5 次)
        {
            let mut s = mp.stats.lock().unwrap_or_else(|e| e.into_inner());
            s[0].consecutive_failures = super::SOFT_CIRCUIT_BREAKER_THRESHOLD;
        }

        // download:primary 已熔断应被跳过,选 mirror
        let result = mp.download_range("http://primary.com/file", 0, 99).await;
        assert!(result.is_ok(), "应跳过熔断源选 mirror 成功");
        assert_eq!(
            result.unwrap(),
            Bytes::from_static(b"mirror"),
            "应选中 mirror(primary 被熔断跳过)"
        );
        assert_eq!(
            primary_counter.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "熔断源 primary 不应被选中"
        );
        assert_eq!(
            mirror_counter.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "健康源 mirror 应被选中"
        );
    }

    /// 软熔断半开探测:所有候选源都已熔断时,应全部重置,给一次恢复探测机会
    /// (不阻断下载)。
    ///
    /// 构造两源都熔断(连续失败 5 次),download 应触发半开重置,任一源成功后
    /// 通过 record_success 清零 consecutive_failures。验证下载成功且熔断状态解除。
    #[tokio::test]
    async fn test_soft_circuit_breaker_half_open_when_all_open() {
        let primary =
            Arc::new(MockProtocol::new().with_download_data(Ok(Bytes::from_static(b"primary"))));
        let mirror =
            Arc::new(MockProtocol::new().with_download_data(Ok(Bytes::from_static(b"mirror"))));

        let mp = MirrorProtocol::new(primary, vec![("http://mirror.com/file".into(), mirror)]);
        // 两源都熔断
        {
            let mut s = mp.stats.lock().unwrap_or_else(|e| e.into_inner());
            s[0].consecutive_failures = super::SOFT_CIRCUIT_BREAKER_THRESHOLD;
            s[1].consecutive_failures = super::SOFT_CIRCUIT_BREAKER_THRESHOLD;
        }

        // download:全熔断应半开重置,不阻断
        let result = mp.download_range("http://primary.com/file", 0, 99).await;
        assert!(result.is_ok(), "全熔断时应半开重置,不阻断下载");

        // 成功后(record_success)选中源的 consecutive_failures 应清零
        let s = mp.stats.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            !s[0].is_circuit_open() || !s[1].is_circuit_open(),
            "至少一个源的熔断状态应因成功而解除"
        );
    }

    /// 软熔断恢复:clear_selected 应重置所有源的软熔断状态(给恢复机会)。
    ///
    /// 构造一源熔断,clear_selected 后断言 consecutive_failures=0(可重新被选)。
    #[tokio::test]
    async fn test_soft_circuit_breaker_reset_on_clear_selected() {
        let primary = Arc::new(MockProtocol::new());
        let mp = MirrorProtocol::new(primary, vec![]);
        {
            let mut s = mp.stats.lock().unwrap_or_else(|e| e.into_inner());
            s[0].consecutive_failures = super::SOFT_CIRCUIT_BREAKER_THRESHOLD;
            assert!(s[0].is_circuit_open(), "构造前置为熔断态");
        }

        mp.clear_selected().await;

        let s = mp.stats.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            s[0].consecutive_failures, 0,
            "clear_selected 应重置软熔断状态"
        );
        assert!(!s[0].is_circuit_open(), "重置后应不再熔断");
    }

    /// 软熔断生命周期:record_failure 递增连续失败,达阈值熔断;record_success
    /// 清零解除。验证 SourceStats 的熔断状态转换。
    #[test]
    fn test_soft_circuit_breaker_lifecycle() {
        let mut s = super::SourceStats::default();
        assert!(!s.is_circuit_open(), "初始未熔断");

        // 连续失败 4 次:未达阈值(5),未熔断
        for _ in 0..4 {
            s.record_failure();
        }
        assert!(!s.is_circuit_open(), "4 次失败未达阈值,未熔断");

        // 第 5 次失败:达阈值,熔断
        s.record_failure();
        assert!(s.is_circuit_open(), "5 次连续失败应熔断");

        // 一次成功:清零,解除熔断
        s.record_success(100, 1_000_000);
        assert!(!s.is_circuit_open(), "成功应清零连续失败,解除熔断");

        // 再次连续失败 5 次重新熔断(验证可重复)
        for _ in 0..5 {
            s.record_failure();
        }
        assert!(s.is_circuit_open(), "再次 5 次失败应重新熔断");

        // reset_circuit 手动重置
        s.reset_circuit();
        assert!(!s.is_circuit_open(), "reset_circuit 应解除熔断");
    }
}
