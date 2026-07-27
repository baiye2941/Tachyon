//! `throughput_baseline` 的 CLI 参数模型与解析
//!
//! 从 `benches/throughput_baseline.rs` 抽出。抽出原因:`benches/` 下全部 bench 都是
//! `harness = false`,其中的 `#[test]` 不会被 `cargo nextest run --all` 收集,参数解析
//! 逻辑无法进入 CI 门禁。抽到本模块后,bench 二进制与 `tests/throughput_baseline_args.rs`
//! 引用同一份源码,测试与实际运行的解析实现完全一致。
//!
//! 本模块只依赖 `std`,不引用 `super::` / `crate::` 中的任何项,故可被集成测试用
//! `#[path]` 原样包含。

// 本模块的公开项按引用方分裂:bench 二进制用 `parse_args` / `print_help`,集成测试
// 只读 `Args` 字段与 `parse_size`。任一编译单元都只用到其中一部分,模块级统一放行。
// 注意:引用方(`#[path] mod ...`)不得再加外层 `#[allow(dead_code)]`,否则触发
// clippy::duplicated_attributes,在 CI 的 -D warnings 下变成错误。
#![allow(dead_code)]

use std::path::PathBuf;

/// 吞吐基线 harness 的运行参数
#[derive(Clone, Debug)]
pub struct Args {
    pub size: u64,
    pub rtt_ms: u64,
    pub bps: u64,
    pub url: Option<String>,
    pub mirrors: Vec<String>,
    pub concurrency: u32,
    pub runs: usize,
    pub out: Option<PathBuf>,
    pub compare_aria2: bool,
    pub aria2_connections: u32,
    /// 强制直连:DownloadConfig.proxy="direct",不读 HTTP_PROXY/系统代理
    pub no_proxy: bool,
    /// 强制 HTTP/1.1(PoolConfig.enable_http2=false),用于代理下 H1/H2 对照
    pub http1_only: bool,
    /// 覆盖 SchedulerConfig.max_fragment_size(MiB)。用于 WAN 多连接对标:
    /// 默认 64MiB 会使 64MiB 文件退化为单分片(peak_conn=1)。
    pub max_frag_mib: Option<u64>,
    /// 本地双源异构:再起一个慢镜像 server(更高 RTT/更低 bps)
    pub local_mirror: bool,
    pub mirror_rtt_ms: u64,
    pub mirror_bps: u64,
    /// 源内慢区间起始偏移(含);与 len/bps 三者齐全才生效
    pub slow_zone_start: Option<u64>,
    /// 源内慢区间长度(字节)
    pub slow_zone_len: Option<u64>,
    /// 源内慢区间带宽上限 bytes/s
    pub slow_zone_bps: Option<u64>,
    /// 禁用动态拆片(`try_rebalance_slowest_fragment`),A/B 量化 on/off 收益用
    pub rebalance_off: bool,
    /// 主源启用 HTTPS(自签证书,客户端 `danger_accept_invalid_certs`)
    pub tls: bool,
    /// 主源 chunk 丢包率 [0.0, 1.0],0=不丢。stream 提前 None = HTTP body 截断
    pub loss_rate: f64,
    /// 镜像源启用 HTTPS
    pub mirror_tls: bool,
    /// 镜像源丢包率 [0.0, 1.0]
    pub mirror_loss_rate: f64,
    /// 镜像源慢区间起始偏移(含);三者齐全才生效
    pub mirror_slow_zone_start: Option<u64>,
    /// 镜像源慢区间长度(字节)
    pub mirror_slow_zone_len: Option<u64>,
    /// 镜像源慢区间带宽上限 bytes/s
    pub mirror_slow_zone_bps: Option<u64>,
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
            slow_zone_start: None,
            slow_zone_len: None,
            slow_zone_bps: None,
            rebalance_off: false,
            tls: false,
            loss_rate: 0.0,
            mirror_tls: false,
            mirror_loss_rate: 0.0,
            mirror_slow_zone_start: None,
            mirror_slow_zone_len: None,
            mirror_slow_zone_bps: None,
        }
    }
}

impl Args {
    /// 组装 `ThrottledServer::start_with_slow_zone` 需要的慢区间元组
    ///
    /// 返回 `(zone_start, zone_end_exclusive, slow_bps)`——第二个元素是**结束偏移
    /// (不含)**而非长度。三个参数齐全才生效,缺任一返回 `None`(注意 `Some(0)`
    /// 是合法起点,判空必须走 `Option` 而非真值判断)。
    pub fn slow_zone(&self) -> Option<(u64, u64, u64)> {
        match (self.slow_zone_start, self.slow_zone_len, self.slow_zone_bps) {
            (Some(start), Some(len), Some(bps)) => Some((start, start.saturating_add(len), bps)),
            _ => None,
        }
    }

    /// 镜像源慢区间元组,语义同 `slow_zone()`。
    pub fn mirror_slow_zone(&self) -> Option<(u64, u64, u64)> {
        match (
            self.mirror_slow_zone_start,
            self.mirror_slow_zone_len,
            self.mirror_slow_zone_bps,
        ) {
            (Some(start), Some(len), Some(bps)) => Some((start, start.saturating_add(len), bps)),
            _ => None,
        }
    }
}

pub fn print_help() {
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
  --slow-zone-start <N[K|M|G]> 本地主源慢区间起始偏移(含)
  --slow-zone-len <N[K|M|G]>  本地主源慢区间长度
  --slow-zone-bps <N[K|M|G]>  本地主源慢区间带宽 bytes/s(三者齐全才生效)
  --mirror-slow-zone-start <N> 镜像源慢区间起始(含)
  --mirror-slow-zone-len <N>  镜像源慢区间长度
  --mirror-slow-zone-bps <N>  镜像源慢区间带宽(三者齐全才生效)
  --tls                       主源启用 HTTPS 自签证书(客户端跳过校验)
  --loss-rate <0.0-1.0>       主源丢包率(chunk drop → body 截断 → 连接中断重试)
  --mirror-tls                镜像源启用 HTTPS
  --mirror-loss-rate <0.0-1.0> 镜像源丢包率
  --rebalance-off             禁用动态拆片(A/B 量化 on/off 收益用)
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
  cargo bench --bench throughput_baseline -- --size 64MiB --bps 50M --slow-zone-start 32MiB --slow-zone-len 8MiB --slow-zone-bps 5M
  cargo bench --bench throughput_baseline -- --url https://.../file.bin --mirror https://mirror/...
"
    );
}

pub fn parse_size(s: &str) -> Result<u64, String> {
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

pub fn parse_args(argv: &[String]) -> Result<Args, String> {
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
            "--slow-zone-start" => {
                i += 1;
                let v = argv.get(i).ok_or("--slow-zone-start 需要参数")?;
                args.slow_zone_start = Some(parse_size(v)?);
            }
            "--slow-zone-len" => {
                i += 1;
                let v = argv.get(i).ok_or("--slow-zone-len 需要参数")?;
                args.slow_zone_len = Some(parse_size(v)?);
            }
            "--slow-zone-bps" => {
                i += 1;
                let v = argv.get(i).ok_or("--slow-zone-bps 需要参数")?;
                args.slow_zone_bps = Some(parse_size(v)?);
            }
            "--mirror-slow-zone-start" => {
                i += 1;
                let v = argv.get(i).ok_or("--mirror-slow-zone-start 需要参数")?;
                args.mirror_slow_zone_start = Some(parse_size(v)?);
            }
            "--mirror-slow-zone-len" => {
                i += 1;
                let v = argv.get(i).ok_or("--mirror-slow-zone-len 需要参数")?;
                args.mirror_slow_zone_len = Some(parse_size(v)?);
            }
            "--mirror-slow-zone-bps" => {
                i += 1;
                let v = argv.get(i).ok_or("--mirror-slow-zone-bps 需要参数")?;
                args.mirror_slow_zone_bps = Some(parse_size(v)?);
            }
            "--tls" => {
                args.tls = true;
            }
            "--mirror-tls" => {
                args.mirror_tls = true;
            }
            "--loss-rate" => {
                i += 1;
                let v = argv.get(i).ok_or("--loss-rate 需要参数")?;
                args.loss_rate = v.parse().map_err(|e| format!("--loss-rate: {e}"))?;
                if !args.loss_rate.is_finite() || !(0.0..=1.0).contains(&args.loss_rate) {
                    return Err(format!(
                        "--loss-rate 必须为 [0.0, 1.0] 范围内的有限数,实际: {v}"
                    ));
                }
            }
            "--mirror-loss-rate" => {
                i += 1;
                let v = argv.get(i).ok_or("--mirror-loss-rate 需要参数")?;
                args.mirror_loss_rate =
                    v.parse().map_err(|e| format!("--mirror-loss-rate: {e}"))?;
                if !args.mirror_loss_rate.is_finite()
                    || !(0.0..=1.0).contains(&args.mirror_loss_rate)
                {
                    return Err(format!(
                        "--mirror-loss-rate 必须为 [0.0, 1.0] 范围内的有限数,实际: {v}"
                    ));
                }
            }
            "--rebalance-off" => {
                args.rebalance_off = true;
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
