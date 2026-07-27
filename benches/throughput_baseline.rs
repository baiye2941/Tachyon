//! 可重复真实吞吐基线 harness
//!
//! 目标:在优化前先量化 goodput / 对齐写命中率 / rebalance / 连接峰值,
//! 分解差距是网络、磁盘还是调度。内部已证明 loopback 下磁盘/调度非主因;
//! 本工具用于 WAN/大文件/多源场景补齐证据。
//!
//! 用法:
//! ```text
//! cargo bench --bench throughput_baseline -- --help
//! cargo bench --bench throughput_baseline -- --size 64MiB --rtt-ms 50 --runs 3
//! cargo bench --bench throughput_baseline -- --url https://cdn.example/file.bin --mirror https://mirror/file.bin
//! ```
//!
//! 编排脚本(场景矩阵 + aria2 对标):
//! `scripts/perf/run_throughput_baseline.ps1` / `.sh`

mod support;

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

// CLI 参数模型抽在 support::baseline_args,由 tests/throughput_baseline_args.rs
// 用 #[path] 包含同一份源码做解析测试(bench 二进制 harness=false,内部 #[test]
// 不会被 nextest 收集)。
use support::baseline_args::{parse_args, print_help};
use support::bench_server::ThrottledServer;
use tachyon_core::Metrics;
use tachyon_core::config::{DownloadConfig, WRITE_BATCH_BYTES};
use tachyon_engine::{BufferPool, ConnectionPool, DownloadTask, PoolConfig};
use tachyon_scheduler::AdaptiveDownloadScheduler;
use tempfile::TempDir;

#[derive(Clone, Debug)]
struct RunResult {
    elapsed_secs: f64,
    bytes: u64,
    goodput_bps: f64,
    fragments_completed: u64,
    errors: u64,
    aligned_passthrough: u64,
    aligned_copied: u64,
    aligned_hit_rate: f64,
    rebalance_count: u64,
    peak_active_requests: u32,
}

fn median_f64(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2.0
    } else {
        values[mid]
    }
}

fn format_bps(bps: f64) -> String {
    if bps >= 1e9 {
        format!("{:.2} GB/s", bps / 1e9)
    } else if bps >= 1e6 {
        format!("{:.2} MB/s", bps / 1e6)
    } else if bps >= 1e3 {
        format!("{:.2} KB/s", bps / 1e3)
    } else {
        format!("{bps:.0} B/s")
    }
}

fn format_bytes(n: u64) -> String {
    if n >= 1024 * 1024 * 1024 {
        format!("{:.2} GiB", n as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if n >= 1024 * 1024 {
        format!("{:.2} MiB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.2} KiB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

fn make_config(dir: &Path, concurrency: u32, no_proxy: bool) -> DownloadConfig {
    let mut config = DownloadConfig::default();
    config.download_dir = dir.to_string_lossy().to_string();
    config.authorized_dirs = vec![config.download_dir.clone()];
    config.max_concurrent_fragments = concurrency;
    // 吞吐基线关闭校验,避免 blake3 整文件读盘污染 goodput
    config.verify_checksum = false;
    // WAN/CDN 慢链路:默认 30s read_timeout 在高延迟并发 Range 下易被
    // reqwest 包装成 "error decoding response body" 假失败;基线给更宽余量。
    config.request_timeout_secs = 120;
    config.connect_timeout_secs = 20;
    config.max_retries = 4;
    if no_proxy {
        // 哨兵:HttpClient 见 "direct" 时 builder.no_proxy(),不读系统代理
        config.proxy = Some("direct".into());
    }
    config
}

/// 构造 DownloadTask 并完成基础注入(buffer_pool / metrics / rebalance 开关 / TLS client)
///
/// 参数:primary_url、mirrors、config、pool、buffer_pool、metrics、sched_config,
/// 加 P0-1/P0-2 A/B 量化新增的 rebalance_off、tls。bench 入口函数(非生产 API),
/// 参数列表直接展开,不引入 builder 间接层。
#[allow(clippy::too_many_arguments)]
async fn build_task(
    primary_url: &str,
    mirrors: &[String],
    config: DownloadConfig,
    pool: Arc<ConnectionPool>,
    buffer_pool: Arc<BufferPool>,
    metrics: Arc<Metrics>,
    sched_config: Option<tachyon_core::config::SchedulerConfig>,
    rebalance_off: bool,
    tls: bool,
) -> Result<DownloadTask, Box<dyn std::error::Error + Send + Sync>> {
    let sc = sched_config.unwrap_or_default();
    let scheduler = Arc::new(AdaptiveDownloadScheduler::new(sc.clone()));

    // TLS bench:用 with_danger_accept_invalid_certs 构造自签证书专用 HttpClient。
    // 注入到 DownloadTask::with_protocol(bench 专用入口),绕过 shared_http_client
    // 的全局 registry(后者会缓存并复用证书校验严格的客户端,不适用于自签 bench)。
    let mut task = if tls {
        use tachyon_core::config::ConnectionConfig;
        let conn = ConnectionConfig::from(pool.config().clone());
        let http = tachyon_protocol::HttpClient::with_danger_accept_invalid_certs(
            &conn,
            config.connect_timeout_secs,
            config.request_timeout_secs,
            config.proxy.as_deref(),
            &config.user_agent,
            &config.headers,
        )
        .map_err(|e| e.to_string())?;
        let http_arc: std::sync::Arc<dyn tachyon_core::traits::Protocol> = if mirrors.is_empty() {
            std::sync::Arc::new(http)
        } else {
            // 镜像源同样用跳过校验的客户端
            let primary = std::sync::Arc::new(http);
            let mirror_protocols: Vec<(
                String,
                std::sync::Arc<dyn tachyon_core::traits::Protocol>,
            )> = mirrors
                .iter()
                .filter_map(|m| {
                    tachyon_protocol::HttpClient::with_danger_accept_invalid_certs(
                        &conn,
                        config.connect_timeout_secs,
                        config.request_timeout_secs,
                        config.proxy.as_deref(),
                        &config.user_agent,
                        &config.headers,
                    )
                    .ok()
                    .map(|c| {
                        (
                            m.clone(),
                            std::sync::Arc::new(c)
                                as std::sync::Arc<dyn tachyon_core::traits::Protocol>,
                        )
                    })
                })
                .collect();
            std::sync::Arc::new(tachyon_engine::MirrorProtocol::with_pool(
                primary,
                mirror_protocols,
                Some(pool.clone()),
            ))
        };
        DownloadTask::with_protocol(
            primary_url.to_string(),
            config,
            Some(pool),
            scheduler,
            http_arc,
        )
        .await
        .map_err(|e| e.to_string())?
    } else if mirrors.is_empty() {
        DownloadTask::with_pool_and_scheduler(
            primary_url.to_string(),
            config,
            Some(pool),
            scheduler,
            None,
        )
        .await
        .map_err(|e| e.to_string())?
    } else {
        DownloadTask::with_mirrors(
            primary_url.to_string(),
            mirrors.to_vec(),
            config,
            Some(pool),
            scheduler,
        )
        .await
        .map_err(|e| e.to_string())?
    };
    task.set_buffer_pool(buffer_pool);
    task.set_metrics(metrics);
    task.set_scheduler_config(sc);
    if rebalance_off {
        task.set_rebalance_enabled(false);
    }
    Ok(task)
}

/// 单次跑一次下载并采集 RunResult(计时 + goodput + retry/rebalance 计数)
///
/// 参数透传到 `build_task`:primary_url、mirrors、concurrency、file_size_hint、
/// no_proxy、http1_only、max_frag_mib,加 P0-1/P0-2 的 rebalance_off、tls。
/// 参数列表与 `build_task` 一致,bench 内部调用,无需 builder。
#[allow(clippy::too_many_arguments)]
async fn run_once(
    primary_url: &str,
    mirrors: &[String],
    concurrency: u32,
    file_size_hint: Option<u64>,
    no_proxy: bool,
    http1_only: bool,
    max_frag_mib: Option<u64>,
    rebalance_off: bool,
    tls: bool,
) -> Result<RunResult, Box<dyn std::error::Error + Send + Sync>> {
    let dir = TempDir::new()?;
    let config = make_config(dir.path(), concurrency, no_proxy);
    let pool = Arc::new(ConnectionPool::new(PoolConfig {
        max_per_host: concurrency.max(1),
        max_global: concurrency.saturating_mul(2).max(8),
        enable_http2: !http1_only,
        ..Default::default()
    }));
    let buffer_pool = Arc::new(BufferPool::with_prefill(
        WRITE_BATCH_BYTES,
        (concurrency as usize).saturating_mul(2).max(8),
    ));
    let metrics = Arc::new(Metrics::new());

    let peak_active = Arc::new(AtomicU32::new(0));
    let peak_flag = Arc::new(AtomicBool::new(true));
    let peak_active_bg = Arc::clone(&peak_active);
    let peak_flag_bg = Arc::clone(&peak_flag);
    let pool_bg = Arc::clone(&pool);
    let sampler = tokio::spawn(async move {
        while peak_flag_bg.load(Ordering::Relaxed) {
            let active = pool_bg.active_requests();
            peak_active_bg.fetch_max(active, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });

    let sched_config = max_frag_mib.map(|mib| {
        use tachyon_core::config::SchedulerConfig;
        let mut sc = SchedulerConfig::default();
        let bytes = mib.saturating_mul(1024 * 1024).max(sc.min_fragment_size);
        sc.max_fragment_size = bytes;
        if sc.min_fragment_size > sc.max_fragment_size {
            sc.min_fragment_size = sc.max_fragment_size;
        }
        sc
    });

    let mut task = build_task(
        primary_url,
        mirrors,
        config,
        pool,
        buffer_pool,
        Arc::clone(&metrics),
        sched_config,
        rebalance_off,
        tls,
    )
    .await?;

    let start = Instant::now();
    task.run().await.map_err(|e| e.to_string())?;
    let elapsed = start.elapsed();
    peak_flag.store(false, Ordering::Relaxed);
    let _ = sampler.await;

    let (bytes, fragments, errors, pass, copy, rebalance, _rebalance_drop) = metrics.snapshot();
    let elapsed_secs = elapsed.as_secs_f64().max(1e-9);
    let effective_bytes = if bytes > 0 {
        bytes
    } else {
        file_size_hint.unwrap_or(0)
    };
    let goodput_bps = effective_bytes as f64 / elapsed_secs;
    let hit = if pass + copy == 0 {
        0.0
    } else {
        pass as f64 / (pass + copy) as f64
    };

    Ok(RunResult {
        elapsed_secs,
        bytes: effective_bytes,
        goodput_bps,
        fragments_completed: fragments,
        errors,
        aligned_passthrough: pass,
        aligned_copied: copy,
        aligned_hit_rate: hit,
        rebalance_count: rebalance,
        peak_active_requests: peak_active.load(Ordering::Relaxed),
    })
}

fn aria2c_bin() -> String {
    std::env::var("ARIA2C").unwrap_or_else(|_| "aria2c".into())
}

fn run_aria2(
    url: &str,
    out_dir: &Path,
    connections: u32,
) -> Result<RunResult, Box<dyn std::error::Error + Send + Sync>> {
    let out_file = out_dir.join("aria2-baseline.bin");
    let _ = std::fs::remove_file(&out_file);
    let start = Instant::now();
    let status = Command::new(aria2c_bin())
        .args([
            "-x",
            &connections.to_string(),
            "-s",
            &connections.to_string(),
            "--allow-overwrite=true",
            "--auto-file-renaming=false",
            // 关闭预分配:与 Tachyon 热路径对比 wall time,避免 Windows 上
            // fallocate/稀疏填充把 aria2 墙钟拉到数秒(平均速假低)。
            "--file-allocation=none",
            "-d",
            out_dir.to_str().ok_or("out_dir utf8")?,
            "-o",
            "aria2-baseline.bin",
            url,
        ])
        .status()?;
    if !status.success() {
        return Err(format!("aria2c 退出码 {status}").into());
    }
    let elapsed = start.elapsed();
    let bytes = std::fs::metadata(&out_file)?.len();
    let elapsed_secs = elapsed.as_secs_f64().max(1e-9);
    Ok(RunResult {
        elapsed_secs,
        bytes,
        goodput_bps: bytes as f64 / elapsed_secs,
        fragments_completed: 0,
        errors: 0,
        aligned_passthrough: 0,
        aligned_copied: 0,
        aligned_hit_rate: 0.0,
        rebalance_count: 0,
        peak_active_requests: connections,
    })
}

fn result_to_json(r: &RunResult) -> String {
    format!(
        "{{\
\"elapsed_secs\":{:.6},\
\"bytes\":{},\
\"goodput_bps\":{:.3},\
\"goodput_human\":\"{}\",\
\"fragments_completed\":{},\
\"errors\":{},\
\"aligned_write_passthrough\":{},\
\"aligned_write_copied\":{},\
\"aligned_write_hit_rate\":{:.6},\
\"rebalance_count\":{},\
\"peak_active_requests\":{}\
}}",
        r.elapsed_secs,
        r.bytes,
        r.goodput_bps,
        format_bps(r.goodput_bps),
        r.fragments_completed,
        r.errors,
        r.aligned_passthrough,
        r.aligned_copied,
        r.aligned_hit_rate,
        r.rebalance_count,
        r.peak_active_requests
    )
}

fn print_run(label: &str, r: &RunResult) {
    println!(
        "[{label}] elapsed={:.3}s bytes={} goodput={} aligned_hit={:.1}% (pass={} copy={}) rebalance={} peak_conn={} frags={} err={}",
        r.elapsed_secs,
        format_bytes(r.bytes),
        format_bps(r.goodput_bps),
        r.aligned_hit_rate * 100.0,
        r.aligned_passthrough,
        r.aligned_copied,
        r.rebalance_count,
        r.peak_active_requests,
        r.fragments_completed,
        r.errors
    );
}

fn serde_json_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `Option<u64>` 序列化为 JSON 数字或 `null`(未配置的 slow_zone 参数)
fn serde_json_opt_u64(v: Option<u64>) -> String {
    v.map_or_else(|| "null".to_string(), |n| n.to_string())
}

fn serde_json_str_arr(items: &[String]) -> String {
    let inner = items
        .iter()
        .map(|s| serde_json_str(s))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{inner}]")
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let filtered: Vec<String> = raw
        .into_iter()
        .filter(|a| a != "--bench" && a != "--test" && !a.starts_with("--nocapture"))
        .collect();

    let args = match parse_args(&filtered) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误: {e}");
            print_help();
            std::process::exit(2);
        }
    };

    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .try_init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");

    let result = rt.block_on(async move {
        let mut server: Option<ThrottledServer> = None;
        let mut mirror_server: Option<ThrottledServer> = None;
        let mut mirrors = args.mirrors.clone();
        let (primary, size_hint) = if let Some(url) = &args.url {
            (url.clone(), None)
        } else {
            // 主源全参数构造:support slow_zone + tls + loss_rate。
            // --rtt-ms 仍走 TTFB(首字节前 sleep),handshake_rtt 暂留 0(预留扩展点)。
            let tls_mode = if args.tls {
                support::bench_server::TlsMode::Tls
            } else {
                support::bench_server::TlsMode::Plaintext
            };
            let s = ThrottledServer::start_with_tls_and_loss(
                args.size,
                args.bps,
                args.rtt_ms,
                0,
                support::bench_server::DEFAULT_CHUNK_SIZE,
                support::bench_server::BenchProtocol::Auto,
                args.slow_zone(),
                tls_mode,
                args.loss_rate,
            )
            .await;
            let url = format!("{}/baseline.bin", s.uri());
            if args.local_mirror {
                let mirror_tls_mode = if args.mirror_tls {
                    support::bench_server::TlsMode::Tls
                } else {
                    support::bench_server::TlsMode::Plaintext
                };
                let ms = ThrottledServer::start_with_tls_and_loss(
                    args.size,
                    args.mirror_bps,
                    args.mirror_rtt_ms,
                    0,
                    support::bench_server::DEFAULT_CHUNK_SIZE,
                    support::bench_server::BenchProtocol::Auto,
                    args.mirror_slow_zone(),
                    mirror_tls_mode,
                    args.mirror_loss_rate,
                )
                .await;
                mirrors.push(format!("{}/baseline.bin", ms.uri()));
                mirror_server = Some(ms);
            }
            server = Some(s);
            (url, Some(args.size))
        };

        println!("=== Tachyon 吞吐基线 ===");
        println!("primary: {primary}");
        if !mirrors.is_empty() {
            println!("mirrors: {mirrors:?}");
        }
        if args.local_mirror {
            println!(
                "local_mirror: rtt_ms={} bps={}",
                args.mirror_rtt_ms, args.mirror_bps
            );
        }
        if let Some((zone_start, zone_end, slow_bps)) = args.slow_zone() {
            println!(
                "slow_zone: [{zone_start}, {zone_end}) bps={slow_bps} ({})",
                format_bps(slow_bps as f64)
            );
        }
        println!(
            "concurrency={} runs={} rtt_ms={} bps={} size_hint={}",
            args.concurrency,
            args.runs,
            args.rtt_ms,
            args.bps,
            size_hint
                .map(format_bytes)
                .unwrap_or_else(|| "unknown".into())
        );
        println!(
            "hint: 热路径 info 已降 debug; RUST_LOG=warn; no_proxy={} http1_only={} max_frag_mib={:?} rebalance_off={} tls={} loss_rate={}",
            args.no_proxy, args.http1_only, args.max_frag_mib, args.rebalance_off, args.tls, args.loss_rate
        );
        println!();

        let mut runs = Vec::with_capacity(args.runs);
        for i in 1..=args.runs {
            let r = run_once(
                &primary,
                &mirrors,
                args.concurrency,
                size_hint,
                args.no_proxy,
                args.http1_only,
                args.max_frag_mib,
                args.rebalance_off,
                args.tls,
            )
            .await?;
            print_run(&format!("tachyon run{i}"), &r);
            runs.push(r);
        }

        let mut goodputs: Vec<f64> = runs.iter().map(|r| r.goodput_bps).collect();
        let median = median_f64(&mut goodputs);
        let last = runs.last().cloned().expect("runs>=1");
        println!();
        println!(
            "tachyon median_goodput={} (n={}) last_aligned_hit={:.1}% last_rebalance={} last_peak_conn={}",
            format_bps(median),
            args.runs,
            last.aligned_hit_rate * 100.0,
            last.rebalance_count,
            last.peak_active_requests
        );

        println!();
        println!("=== 差距分解提示 ===");
        if args.bps > 0 {
            let theoretical = args.bps as f64;
            let util = (median / theoretical * 100.0).min(999.0);
            println!(
                "相对本地 server 带宽上限: median/limit = {:.1}% (limit={})",
                util,
                format_bps(theoretical)
            );
            if util > 95.0 {
                println!("→ 网络/节流上限已打满,磁盘/调度非主因");
            } else if last.aligned_hit_rate < 0.5 && last.aligned_copied > 0 {
                println!("→ aligned_write_copied 偏高,检查大 chunk 直写对齐/BufferPool 注入");
            } else if last.peak_active_requests <= 1 && args.concurrency > 1 {
                println!("→ 峰值连接≈1,调度/并发未展开(或单连接 CDN 限流)");
            } else {
                println!("→ 未打满上限:查 RTT/丢包/并发/源限流;对照 aria2");
            }
        } else if last.aligned_hit_rate < 0.5 && last.aligned_copied > last.aligned_passthrough {
            println!("→ 对齐直写命中率低(copied 主导),WAN 上可能被淹没但 loopback 会吃拷贝");
        } else {
            println!("→ 无本地带宽上限;用 --compare-aria2 或 CDN/本地异构镜像对照");
        }

        let mut aria2_result = None;
        if args.compare_aria2 {
            let aria_dir = TempDir::new()?;
            match run_aria2(&primary, aria_dir.path(), args.aria2_connections) {
                Ok(ar) => {
                    print_run("aria2", &ar);
                    let ratio = if ar.goodput_bps > 0.0 {
                        median / ar.goodput_bps
                    } else {
                        0.0
                    };
                    println!("tachyon/aria2 goodput ratio = {ratio:.2}x (1.0=持平)");
                    aria2_result = Some(ar);
                }
                Err(e) => {
                    eprintln!("aria2 对标失败(可忽略): {e}");
                }
            }
        }

        if let Some(path) = &args.out {
            let body = format!(
                "{{\
\"primary\":{},\
\"mirrors\":{},\
\"concurrency\":{},\
\"runs\":{},\
\"rtt_ms\":{},\
\"bps\":{},\
\"local_mirror\":{},\
\"mirror_rtt_ms\":{},\
\"mirror_bps\":{},\
\"slow_zone_start\":{},\
\"slow_zone_len\":{},\
\"slow_zone_bps\":{},\
\"tls\":{},\
\"loss_rate\":{:.4},\
\"mirror_tls\":{},\
\"mirror_loss_rate\":{:.4},\
\"mirror_slow_zone_start\":{},\
\"mirror_slow_zone_len\":{},\
\"mirror_slow_zone_bps\":{},\
\"rebalance_off\":{},\
\"median_goodput_bps\":{:.3},\
\"last\":{},\
\"aria2\":{}\
}}",
                serde_json_str(&primary),
                serde_json_str_arr(&mirrors),
                args.concurrency,
                args.runs,
                args.rtt_ms,
                args.bps,
                args.local_mirror,
                args.mirror_rtt_ms,
                args.mirror_bps,
                serde_json_opt_u64(args.slow_zone_start),
                serde_json_opt_u64(args.slow_zone_len),
                serde_json_opt_u64(args.slow_zone_bps),
                args.tls,
                args.loss_rate,
                args.mirror_tls,
                args.mirror_loss_rate,
                serde_json_opt_u64(args.mirror_slow_zone_start),
                serde_json_opt_u64(args.mirror_slow_zone_len),
                serde_json_opt_u64(args.mirror_slow_zone_bps),
                args.rebalance_off,
                median,
                result_to_json(&last),
                aria2_result
                    .as_ref()
                    .map(result_to_json)
                    .unwrap_or_else(|| "null".into())
            );
            std::fs::write(path, body)?;
            println!("JSON 已写: {}", path.display());
        }

        if let Some(mut s) = server {
            s.shutdown();
        }
        if let Some(mut s) = mirror_server {
            s.shutdown();
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });

    if let Err(e) = result {
        eprintln!("baseline 失败: {e}");
        std::process::exit(1);
    }
}
