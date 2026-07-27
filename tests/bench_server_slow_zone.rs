//! bench harness「源内分区间限速」(slow_zone)行为测试
//!
//! 锁定 `ThrottledServer::start_with_slow_zone` 的契约:同一个源的指定字节区间
//! 以更低速率响应,使「某个分片恰好落在劣化区间」(straggler)的场景可被基准复现。
//!
//! # 判据(来自需求)
//!
//! 慢区间配置为 `Option<(zone_start, zone_end_exclusive, slow_bps)>`。是否降速
//! **只看请求 Range 的 start 偏移**,而非逐字节切换 —— 真实服务端限速是按连接的,
//! 一个连接要么快要么慢。因此:
//! - start ∈ [zone_start, zone_end) → 整个响应用 slow_bps
//! - start ∉ [zone_start, zone_end) → 整个响应用全局 bytes_per_sec
//!
//! # 计时断言的噪声余量
//!
//! `throttled_stream` 的节流是「每 chunk 后 sleep(chunk_size / bps)」。本测试用
//! 64KiB chunk、256KiB 请求区间(4 chunk → 3 次 sleep):
//! - 快区间 16MiB/s → 理论每 chunk ≈ 3.9ms,合计 ≈ 12ms
//! - 慢区间 512KiB/s → 每 chunk = 125ms,合计 = 375ms
//!
//! Windows 定时器粒度约 15.6ms,会把快区间的 3.9ms sleep 抬高到粒度下限,因此快
//! 区间实测比理论值大。本机(Windows)实测:快 = 42.3ms、慢 = 437.9ms,比值 10.4x;
//! Linux(1ms 粒度)下快区间更接近理论值,比值只会更大。断言阈值取 2x,即使在最差
//! 的 Windows 粒度下也留有 5 倍余量:要让断言误判,需要快区间请求膨胀到 220ms 以上。
//!
//! 所有计时断言均为「同一测试内、同一 client 的相对比较」,不含任何绝对墙钟阈值,
//! 故与机器绝对性能无关。

// 复用 bench 侧的节流 server 实现。`benches/support/bench_server.rs` 原本挂在
// `benches/support/mod.rs` 之下,但 benches/ 的全部 bench 都是 harness = false,
// 其中的 #[test] 不会被 `cargo nextest run --all` 收集;因此用 #[path] 把同一份
// 源码直接包含进本集成测试单元,保证测试与 bench 跑的是完全相同的实现(而不是
// 复制一份会漂移的副本)。该文件不引用 super::/crate:: 中的任何项(只用 std 与
// 外部 crate),可原样包含,无需补任何等价定义。
// 注意:此处不能再加外层 `#[allow(dead_code)]`。bench_server.rs 内部第 21 行已有
// 模块级 `#![allow(dead_code)]`,足以覆盖「本测试只用到少数公开项」造成的死代码
// 告警(实测本编译单元零 dead_code 警告);再加外层 allow 会触发
// clippy::duplicated_attributes,在 CI 的 -D warnings 下直接变成错误。
#[path = "../benches/support/bench_server.rs"]
mod bench_server;

use std::time::{Duration, Instant};

use bench_server::ThrottledServer;
use bytes::Bytes;
use tachyon_core::traits::Protocol;
use tachyon_protocol::HttpClient;

/// 模拟文件总大小(8MiB,容纳慢区间及其两侧的快区间)
const FILE_SIZE: u64 = 8 * 1024 * 1024;
/// 慢区间起始偏移(含)
const ZONE_START: u64 = 4 * 1024 * 1024;
/// 慢区间长度
const ZONE_LEN: u64 = 1024 * 1024;
/// 慢区间结束偏移(不含)
const ZONE_END: u64 = ZONE_START + ZONE_LEN;
/// 单次测量的请求区间长度(256KiB = 4 个 64KiB chunk → 3 次节流 sleep)
const RANGE_LEN: u64 = 256 * 1024;
/// 全局带宽(快):每 chunk 约 3.9ms
const FAST_BPS: u64 = 16 * 1024 * 1024;
/// 慢区间带宽:每 chunk 125ms
const SLOW_BPS: u64 = 512 * 1024;
/// 模拟 RTT(0 = 无首字节延迟,使计时只反映节流差异)
const RTT_MS: u64 = 0;
/// 远离慢区间的快区间基线起点
const FAST_RANGE_START: u64 = 0;
/// 慢/快耗时比值阈值。设计比值约 32x,取 2x 留足噪声余量
const SLOW_FACTOR: u32 = 2;
/// 「行为不变」回归保护的耗时容忍倍数(未配置慢区间时不应出现数量级劣化)
const PARITY_FACTOR: u32 = 4;
/// 「行为不变」回归保护的绝对松弛量(吸收进程调度抖动)
const PARITY_SLACK: Duration = Duration::from_millis(200);

/// 构造走 tachyon-protocol 的 HTTP 客户端(禁止自建 reqwest Client 绕过协议层)
fn client() -> HttpClient {
    HttpClient::with_timeouts(5, 30, None).expect("HttpClient 构造应成功")
}

/// 预热:先发一个 1KiB 请求建立 keep-alive 连接
///
/// 把 TCP 连接建立 + HTTP 协商的一次性开销移出计时窗口,使后续测量只反映节流差异。
/// 1KiB < chunk_size,只有 1 个 chunk,无论落在快慢区间都不产生节流 sleep。
async fn warm_up(client: &HttpClient, url: &str) {
    client
        .download_range(url, 0, 1023, None)
        .await
        .expect("预热请求应成功");
}

/// 发一次 Range 请求并计时,返回(响应体, 墙钟耗时)
async fn timed_range(client: &HttpClient, url: &str, start: u64, len: u64) -> (Bytes, Duration) {
    let end = start + len - 1;
    let started = Instant::now();
    let body = client
        .download_range(url, start, end, None)
        .await
        .expect("Range 请求应成功");
    let elapsed = started.elapsed();
    assert_eq!(
        body.len() as u64,
        len,
        "Range [{start}, {end}] 的响应长度应等于请求长度"
    );
    (body, elapsed)
}

/// 断言 `slow` 显著慢于 `fast`(至少 SLOW_FACTOR 倍)
fn assert_significantly_slower(slow: Duration, fast: Duration, scenario: &str) {
    assert!(
        slow >= fast * SLOW_FACTOR,
        "{scenario}: 慢区间耗时应至少为快区间的 {SLOW_FACTOR} 倍,实测 慢={slow:?} 快={fast:?}"
    );
}

/// slow_zone = None 时,行为与既有 `start()` 逐字节一致(回归保护)
///
/// 锁定需求第 3 条:新增构造函数不得改变现有 bench 依赖的行为。同时验证「若配置了
/// 慢区间本应降速」的那段偏移,在 None 下没有被误降速。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bench_server_slow_zone_none_behaves_like_start() {
    let mut baseline = ThrottledServer::start(FILE_SIZE, FAST_BPS, RTT_MS).await;
    let mut probe = ThrottledServer::start_with_slow_zone(FILE_SIZE, FAST_BPS, RTT_MS, None).await;

    let client = client();
    let baseline_url = format!("{}/bench.bin", baseline.uri());
    let probe_url = format!("{}/bench.bin", probe.uri());
    warm_up(&client, &baseline_url).await;
    warm_up(&client, &probe_url).await;

    // 取「若配置慢区间则会降速」的那段偏移,确保 None 分支确实没走降速路径
    let (baseline_body, baseline_elapsed) =
        timed_range(&client, &baseline_url, ZONE_START, RANGE_LEN).await;
    let (probe_body, probe_elapsed) = timed_range(&client, &probe_url, ZONE_START, RANGE_LEN).await;

    assert_eq!(
        baseline_body, probe_body,
        "slow_zone = None 的响应体应与 start() 逐字节一致"
    );
    assert!(
        probe_elapsed <= baseline_elapsed * PARITY_FACTOR + PARITY_SLACK,
        "slow_zone = None 不应引入节流劣化,实测 None={probe_elapsed:?} start()={baseline_elapsed:?}"
    );

    probe.shutdown();
    baseline.shutdown();
}

/// 配置慢区间后,区间内的 Range 显著慢于同长度的区间外 Range
///
/// 这是 Task 2/3 straggler 收益验证的核心前置能力。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bench_server_slow_zone_inside_range_is_slower_than_outside() {
    let mut server = ThrottledServer::start_with_slow_zone(
        FILE_SIZE,
        FAST_BPS,
        RTT_MS,
        Some((ZONE_START, ZONE_END, SLOW_BPS)),
    )
    .await;

    let client = client();
    let url = format!("{}/bench.bin", server.uri());
    warm_up(&client, &url).await;

    // 慢区间正中的一段,完整落在 [ZONE_START, ZONE_END) 内
    let inside_start = ZONE_START + ZONE_LEN / 2;
    let (_, fast_elapsed) = timed_range(&client, &url, FAST_RANGE_START, RANGE_LEN).await;
    let (_, slow_elapsed) = timed_range(&client, &url, inside_start, RANGE_LEN).await;

    assert_significantly_slower(slow_elapsed, fast_elapsed, "慢区间内部 Range");

    server.shutdown();
}

/// 边界:请求恰好起始于 zone_start 时应降速(下界为闭区间)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bench_server_slow_zone_boundary_at_zone_start_is_slow() {
    let mut server = ThrottledServer::start_with_slow_zone(
        FILE_SIZE,
        FAST_BPS,
        RTT_MS,
        Some((ZONE_START, ZONE_END, SLOW_BPS)),
    )
    .await;

    let client = client();
    let url = format!("{}/bench.bin", server.uri());
    warm_up(&client, &url).await;

    let (_, fast_elapsed) = timed_range(&client, &url, FAST_RANGE_START, RANGE_LEN).await;
    let (_, slow_elapsed) = timed_range(&client, &url, ZONE_START, RANGE_LEN).await;

    assert_significantly_slower(slow_elapsed, fast_elapsed, "起始于 zone_start 的 Range");

    server.shutdown();
}

/// 边界:请求恰好起始于 zone_end - 1 时应降速
///
/// 该请求的区间只有第一个字节落在慢区间内、其余部分越出区间。按「只看 start」的
/// 判据,整个响应仍必须走 slow_bps。这条用例专门排除「要求区间完全包含于慢区间」
/// 的错误实现。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bench_server_slow_zone_boundary_at_last_byte_is_slow() {
    let mut server = ThrottledServer::start_with_slow_zone(
        FILE_SIZE,
        FAST_BPS,
        RTT_MS,
        Some((ZONE_START, ZONE_END, SLOW_BPS)),
    )
    .await;

    let client = client();
    let url = format!("{}/bench.bin", server.uri());
    warm_up(&client, &url).await;

    let (_, fast_elapsed) = timed_range(&client, &url, FAST_RANGE_START, RANGE_LEN).await;
    let (_, slow_elapsed) = timed_range(&client, &url, ZONE_END - 1, RANGE_LEN).await;

    assert_significantly_slower(slow_elapsed, fast_elapsed, "起始于 zone_end - 1 的 Range");

    server.shutdown();
}

/// 边界:请求恰好起始于 zone_end 时应保持快速(上界为开区间)
///
/// 这条用例专门排除「把 zone_end 当作闭区间上界」的错误实现。断言用同一 server 内
/// 的慢区间请求作参照,避免任何绝对墙钟阈值。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bench_server_slow_zone_boundary_at_zone_end_is_fast() {
    let mut server = ThrottledServer::start_with_slow_zone(
        FILE_SIZE,
        FAST_BPS,
        RTT_MS,
        Some((ZONE_START, ZONE_END, SLOW_BPS)),
    )
    .await;

    let client = client();
    let url = format!("{}/bench.bin", server.uri());
    warm_up(&client, &url).await;

    let (_, slow_elapsed) = timed_range(&client, &url, ZONE_START, RANGE_LEN).await;
    let (_, boundary_elapsed) = timed_range(&client, &url, ZONE_END, RANGE_LEN).await;

    assert_significantly_slower(slow_elapsed, boundary_elapsed, "起始于 zone_end 的 Range");

    server.shutdown();
}

/// 降速只改变时序,不改变字节内容(纯确定性断言,不含计时)
///
/// 慢区间响应体必须与未配置慢区间的 server 逐字节一致,确保 bench 仍能对下载结果
/// 做哈希校验。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bench_server_slow_zone_payload_matches_unthrottled_baseline() {
    let mut baseline = ThrottledServer::start(FILE_SIZE, FAST_BPS, RTT_MS).await;
    let mut throttled = ThrottledServer::start_with_slow_zone(
        FILE_SIZE,
        FAST_BPS,
        RTT_MS,
        Some((ZONE_START, ZONE_END, SLOW_BPS)),
    )
    .await;

    let client = client();
    let baseline_url = format!("{}/bench.bin", baseline.uri());
    let throttled_url = format!("{}/bench.bin", throttled.uri());

    let (baseline_body, _) = timed_range(&client, &baseline_url, ZONE_START, RANGE_LEN).await;
    let (throttled_body, _) = timed_range(&client, &throttled_url, ZONE_START, RANGE_LEN).await;

    assert_eq!(
        baseline_body, throttled_body,
        "慢区间只应影响时序,响应字节必须与未节流 server 完全一致"
    );

    throttled.shutdown();
    baseline.shutdown();
}

// ── P0-2:TLS + 丢包模拟行为契约 ────────────────────────────────────────

/// TLS 自签证书 server + 客户端 `danger_accept_invalid_certs(true)` 能完成完整下载。
///
/// 锁定契约:TlsMode::Tls 的 server 经 rcgen 自签证书启动,reqwest 客户端跳过证书
/// 校验后能拿到完整 body(字节内容与明文 server 一致)。若 rcgen/rustls/tokio-rustls
/// 版本升级导致证书生成或 TLS 握手路径回归,此测试会失败。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bench_server_tls_handshake_completes_and_payload_matches_plaintext() {
    use bench_server::{BenchProtocol, DEFAULT_CHUNK_SIZE, TlsMode};

    // rustls 0.23 需显式安装 CryptoProvider(进程级,幂等)。多测试并发安装会 panic,
    // 用 try_install 忽略"已安装"错误。reqwest 经 rustls/ring feature 也用同一 provider。
    let _ = rustls::crypto::ring::default_provider().install_default();

    let size = 256 * 1024u64; // 4 个 chunk,无 slow_zone 无丢包
    let bps = 16 * 1024 * 1024; // 不限速实质(快)
    let mut plain = ThrottledServer::start_with_tls_and_loss(
        size,
        bps,
        0,
        0,
        DEFAULT_CHUNK_SIZE,
        BenchProtocol::Auto,
        None,
        TlsMode::Plaintext,
        0.0,
    )
    .await;
    let mut tls = ThrottledServer::start_with_tls_and_loss(
        size,
        bps,
        0,
        0,
        DEFAULT_CHUNK_SIZE,
        BenchProtocol::Auto,
        None,
        TlsMode::Tls,
        0.0,
    )
    .await;

    // 客户端:用 with_danger_accept_invalid_certs 跳过自签证书校验
    use tachyon_core::config::ConnectionConfig;
    let conn = ConnectionConfig::default();
    let tls_client = HttpClient::with_danger_accept_invalid_certs(
        &conn,
        5,
        30,
        None,
        "tachyon-bench-test",
        &std::collections::HashMap::new(),
    )
    .expect("TLS 跳过校验客户端构造应成功");

    let plain_url = format!("{}/bench.bin", plain.uri());
    let tls_url = format!("{}/bench.bin", tls.uri());

    // 全量下载(无 Range)
    let plain_body = tls_client
        .download_range(&plain_url, 0, size - 1, None)
        .await
        .expect("明文下载应成功");
    let tls_body = tls_client
        .download_range(&tls_url, 0, size - 1, None)
        .await
        .expect("TLS 自签 + 跳过校验下载应成功");

    assert_eq!(
        plain_body.len(),
        size as usize,
        "明文 body 长度应等于请求区间"
    );
    assert_eq!(tls_body.len(), size as usize, "TLS body 长度应等于请求区间");
    assert_eq!(
        plain_body, tls_body,
        "TLS 与明文 server 的字节内容必须一致(同一确定性填充)"
    );

    tls.shutdown();
    plain.shutdown();
}

/// `loss_rate=1.0` 时,stream 必须在首个非首 chunk 前以 Err 终止,
/// 客户端收到不完整 body 报 connection error(触发 downloader 分片重试/续传路径)。
///
/// 锁定契约:loss_rate > 0 时 `throttled_stream` 会按概率丢 chunk,body 截断而非
/// 完整返回。loss_rate=1.0 是确定性丢包(除首 chunk 外必丢),便于断言。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bench_server_loss_rate_truncates_body_at_full_loss() {
    use bench_server::{BenchProtocol, DEFAULT_CHUNK_SIZE, TlsMode};

    let size = 256 * 1024u64; // 4 个 64KiB chunk
    let bps = 16 * 1024 * 1024; // 节流不影响丢包判定
    let mut lossy = ThrottledServer::start_with_tls_and_loss(
        size,
        bps,
        0,
        0,
        DEFAULT_CHUNK_SIZE,
        BenchProtocol::Auto,
        None,
        TlsMode::Plaintext,
        1.0, // ← 全丢(除首 chunk)
    )
    .await;

    let c = client();
    let url = format!("{}/bench.bin", lossy.uri());

    // 全量 Range 请求:首 chunk 收到后,第二个 chunk 必丢 → body 截断
    let result = c.download_range(&url, 0, size - 1, None).await;

    assert!(
        result.is_err(),
        "loss_rate=1.0 应使 body 截断,客户端报 connection error,实际: {:?}",
        result
    );

    lossy.shutdown();
}

/// `loss_rate=0.0` 时,行为与既有 `start()` 完全一致(回归保护)。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bench_server_zero_loss_rate_behaves_like_start() {
    use bench_server::{BenchProtocol, DEFAULT_CHUNK_SIZE, TlsMode};

    let size = 256 * 1024u64;
    let bps = 16 * 1024 * 1024;
    let mut baseline = ThrottledServer::start(size, bps, 0).await;
    let mut zero_loss = ThrottledServer::start_with_tls_and_loss(
        size,
        bps,
        0,
        0,
        DEFAULT_CHUNK_SIZE,
        BenchProtocol::Auto,
        None,
        TlsMode::Plaintext,
        0.0,
    )
    .await;

    let c = client();
    let baseline_url = format!("{}/bench.bin", baseline.uri());
    let zero_loss_url = format!("{}/bench.bin", zero_loss.uri());

    let baseline_body = c
        .download_range(&baseline_url, 0, size - 1, None)
        .await
        .expect("baseline 应成功");
    let zero_loss_body = c
        .download_range(&zero_loss_url, 0, size - 1, None)
        .await
        .expect("loss_rate=0.0 应成功(等价 baseline)");

    assert_eq!(
        baseline_body, zero_loss_body,
        "loss_rate=0.0 应与 baseline 字节一致"
    );

    zero_loss.shutdown();
    baseline.shutdown();
}
