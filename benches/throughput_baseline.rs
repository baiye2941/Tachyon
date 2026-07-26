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

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use support::bench_server::ThrottledServer;
use tachyon_core::Metrics;
use tachyon_core::config::{DownloadConfig, WRITE_BATCH_BYTES};
use tachyon_engine::{BufferPool, ConnectionPool, DownloadTask, PoolConfig};
use tachyon_scheduler::AdaptiveDownloadScheduler;
use tempfile::TempDir;

#[derive(Clone, Debug)]
struct Args {
    size: u64,
    rtt_ms: u64,
    bps: u64,
    url: Option<String>,
    mirrors: Vec<String>,
    concurrency: u32,
    runs: usize,
    out: Option<PathBuf>,
    compare_aria2: bool,
    aria2_connections: u32,
    /// 强制直连:DownloadConfig.proxy="direct",不读 HTTP_PROXY/系统代理
    no_proxy: bool,
    /// 强制 HTTP/1.1(PoolConfig.enable_http2=false),用于代理下 H1/H2 对照
    http1_only: bool,
    /// 覆盖 SchedulerConfig.max_fragment_size(MiB)。用于 WAN 多连接对标:
    /// 默认 64MiB 会使 64MiB 文件退化为单分片(peak_conn=1)。
    max_frag_mib: Option<u64>,
    /// 本地双源异构:再起一个慢镜像 server(更高 RTT/更低 bps)
    local_mirror: bool,
    mirror_rtt_ms: u64,
    mirror_bps: u64,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            size: 64 * 1024 * 1024,
            rtt_ms: 0,
            bps: 0,
            url: None,
            mirrors: Vec::new(),
            concurrency: 16,
            runs: 3,
            out: None,
            compare_aria2: false,
            aria2_connections: 16,
            no_proxy: false,
            http1_only: false,
            max_frag_mib: None,
            local_mirror: false,
            mirror_rtt_ms: 200,
            mirror_bps: 5_000_000, // 5MB/s 慢源
        }
    }
}

fn print_help() {
    eprintln!(
        "\
Tachyon 吞吐基线 harness

OPTIONS:
  --size <N[K|M|G|MiB|GiB]>   本地 server 文件大小 (默认 64MiB)
  --rtt-ms <u64>              本地主源模拟 RTT 毫秒 (默认 0)
  --bps <N[K|M|G]>            本地主源带宽上限 bytes/s,0=不限 (默认 0)
  --url <URL>                 外部主源(跳过本地 server)
  --mirror <URL>              镜像源,可重复
  --local-mirror              启动本地慢镜像源(异构 RTT/bps,测 rebalance)
  --mirror-rtt-ms <u64>       本地慢镜像 RTT 毫秒 (默认 200)
  --mirror-bps <N[K|M|G]>     本地慢镜像带宽 (默认 5M)
  --concurrency <u32>         max_concurrent_fragments (默认 16)
  --runs <usize>              重复次数 (默认 3)
  --out <path>                写出 JSON 结果
  --compare-aria2             同机 aria2c -xN -sN 对标(需安装)
  --aria2-connections <u32>   aria2 -x/-s (默认 16)
  --no-proxy                  强制直连(proxy=direct),忽略 HTTP_PROXY/系统代理
  --http1-only                强制 HTTP/1.1(禁用 H2),代理下对照用
  --max-frag-mib <u64>        覆盖 max_fragment_size(MiB),强制多分片多连接
  --help                      显示帮助

示例:
  cargo bench --bench throughput_baseline -- --size 512MiB --runs 3
  cargo bench --bench throughput_baseline -- --size 64MiB --rtt-ms 100 --bps 50M
  cargo bench --bench throughput_baseline -- --size 64MiB --local-mirror --mirror-rtt-ms 200
  cargo bench --bench throughput_baseline -- --url https://.../file.bin --mirror https://mirror/...
"
    );
}

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let (num, mul) = if let Some(n) = lower.strip_suffix("gib") {
        (n, 1024u64 * 1024 * 1024)
    } else if let Some(n) = lower.strip_suffix("mib") {
        (n, 1024 * 1024)
    } else if let Some(n) = lower.strip_suffix("kib") {
        (n, 1024)
    } else if let Some(n) = lower.strip_suffix('g') {
        (n, 1000 * 1000 * 1000)
    } else if let Some(n) = lower.strip_suffix('m') {
        (n, 1000 * 1000)
    } else if let Some(n) = lower.strip_suffix('k') {
        (n, 1000)
    } else {
        (s, 1)
    };
    let num = num.trim();
    // 支持小数: 12.5M / 0.5GiB; 整数路径仍走 u64 避免浮点误差
    if num.contains('.') {
        let v: f64 = num.parse().map_err(|e| format!("无效大小 '{s}': {e}"))?;
        if !v.is_finite() || v < 0.0 {
            return Err(format!("无效大小 '{s}': 必须为非负有限数"));
        }
        let product = v * mul as f64;
        if product > u64::MAX as f64 {
            return Err(format!("无效大小 '{s}': 溢出 u64"));
        }
        Ok(product.round() as u64)
    } else {
        num.parse::<u64>()
            .map(|v| v.saturating_mul(mul))
            .map_err(|e| format!("无效大小 '{s}': {e}"))
    }
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args::default();
    let mut i = 0usize;
    while i < argv.len() {
        let a = &argv[i];
        match a.as_str() {
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--size" => {
                i += 1;
                let v = argv.get(i).ok_or("--size 需要参数")?;
                args.size = parse_size(v)?;
            }
            "--rtt-ms" => {
                i += 1;
                args.rtt_ms = argv
                    .get(i)
                    .ok_or("--rtt-ms 需要参数")?
                    .parse()
                    .map_err(|e| format!("--rtt-ms: {e}"))?;
            }
            "--bps" => {
                i += 1;
                let v = argv.get(i).ok_or("--bps 需要参数")?;
                args.bps = parse_size(v)?;
            }
            "--url" => {
                i += 1;
                args.url = Some(argv.get(i).ok_or("--url 需要参数")?.clone());
            }
            "--mirror" => {
                i += 1;
                args.mirrors
                    .push(argv.get(i).ok_or("--mirror 需要参数")?.clone());
            }
            "--concurrency" => {
                i += 1;
                args.concurrency = argv
                    .get(i)
                    .ok_or("--concurrency 需要参数")?
                    .parse()
                    .map_err(|e| format!("--concurrency: {e}"))?;
            }
            "--runs" => {
                i += 1;
                args.runs = argv
                    .get(i)
                    .ok_or("--runs 需要参数")?
                    .parse()
                    .map_err(|e| format!("--runs: {e}"))?;
                if args.runs == 0 {
                    return Err("--runs 必须 >= 1".into());
                }
            }
            "--out" => {
                i += 1;
                args.out = Some(PathBuf::from(argv.get(i).ok_or("--out 需要参数")?));
            }
            "--compare-aria2" => {
                args.compare_aria2 = true;
            }
            "--local-mirror" => {
                args.local_mirror = true;
            }
            "--mirror-rtt-ms" => {
                i += 1;
                args.mirror_rtt_ms = argv
                    .get(i)
                    .ok_or("--mirror-rtt-ms 需要参数")?
                    .parse()
                    .map_err(|e| format!("--mirror-rtt-ms: {e}"))?;
            }
            "--mirror-bps" => {
                i += 1;
                let v = argv.get(i).ok_or("--mirror-bps 需要参数")?;
                args.mirror_bps = parse_size(v)?;
            }
            "--aria2-connections" => {
                i += 1;
                args.aria2_connections = argv
                    .get(i)
                    .ok_or("--aria2-connections 需要参数")?
                    .parse()
                    .map_err(|e| format!("--aria2-connections: {e}"))?;
            }
            "--no-proxy" => {
                args.no_proxy = true;
            }
            "--http1-only" => {
                args.http1_only = true;
            }
            "--max-frag-mib" => {
                i += 1;
                args.max_frag_mib = Some(
                    argv.get(i)
                        .ok_or("--max-frag-mib 需要参数")?
                        .parse()
                        .map_err(|e| format!("--max-frag-mib: {e}"))?,
                );
                if args.max_frag_mib == Some(0) {
                    return Err("--max-frag-mib 必须 >= 1".into());
                }
            }
            other if other.starts_with('-') => {
                return Err(format!("未知参数: {other} (试 --help)"));
            }
            _ => return Err(format!("位置参数未支持: {a}")),
        }
        i += 1;
    }
    Ok(args)
}

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

async fn build_task(
    primary_url: &str,
    mirrors: &[String],
    config: DownloadConfig,
    pool: Arc<ConnectionPool>,
    buffer_pool: Arc<BufferPool>,
    metrics: Arc<Metrics>,
    sched_config: Option<tachyon_core::config::SchedulerConfig>,
) -> Result<DownloadTask, Box<dyn std::error::Error + Send + Sync>> {
    let sc = sched_config.unwrap_or_default();
    let scheduler = Arc::new(AdaptiveDownloadScheduler::new(sc.clone()));
    let mut task = if mirrors.is_empty() {
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
    Ok(task)
}

async fn run_once(
    primary_url: &str,
    mirrors: &[String],
    concurrency: u32,
    file_size_hint: Option<u64>,
    no_proxy: bool,
    http1_only: bool,
    max_frag_mib: Option<u64>,
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
            let s = ThrottledServer::start(args.size, args.bps, args.rtt_ms).await;
            let url = format!("{}/baseline.bin", s.uri());
            if args.local_mirror {
                let ms =
                    ThrottledServer::start(args.size, args.mirror_bps, args.mirror_rtt_ms).await;
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
            "hint: 热路径 info 已降 debug; RUST_LOG=warn; no_proxy={} http1_only={} max_frag_mib={:?}",
            args.no_proxy, args.http1_only, args.max_frag_mib
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
