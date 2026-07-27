//! `throughput_baseline` CLI 参数解析测试 —— 覆盖 slow-zone 三参数 + 搬迁回归保护
//!
//! 对应 brief「测试要求」最后一条(CLI 参数解析:三参数齐全 / 缺一个 / 数值格式错误)。
//!
//! # 为什么测试在这里而不在 bench 里
//!
//! `benches/throughput_baseline.rs` 是 `harness = false` 的 bench 二进制,其中的
//! `#[cfg(test)] mod tests` 不会被 `cargo nextest run --all` 收集(criterion 的
//! `criterion_main!` 接管了 test harness)。故把 `Args` / `parse_size` / `parse_args` /
//! `print_help` 抽到 `benches/support/baseline_args.rs`,再由本集成测试用 `#[path]`
//! 包含同一份源码 —— 测试与 bench 跑的是完全相同的实现,且真正进入 CI 门禁。
//!
//! # 未覆盖路径(有意为之)
//!
//! - `--help` / `-h`:`parse_args` 该分支调用 `std::process::exit(0)`,在测试进程中
//!   会直接杀死进程(而非返回),无法在同进程内断言。属 CLI 惯例路径,由验收步骤
//!   `cargo bench --bench throughput_baseline -- --help` 人工确认。
//! - `print_help()`:纯 stderr 输出,无返回值可断言。

// 与 tests/bench_server_slow_zone.rs 同一手法。此处同样**不能**加外层
// `#[allow(dead_code)]`:baseline_args.rs 首行已有模块级 `#![allow(dead_code)]`,
// 再加外层会触发 clippy::duplicated_attributes,在 CI 的 -D warnings 下变成错误
// (上一轮已实测复现)。内层 allow 足以覆盖本编译单元里未被引用的 pub 项
// (如 print_help、部分 Args 字段)。
#[path = "../benches/support/baseline_args.rs"]
mod baseline_args;

use baseline_args::{Args, parse_args, parse_size};

/// 把 `&[&str]` 转成 `parse_args` 需要的 `&[String]`
fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// 解析并断言成功
fn parse_ok(items: &[&str]) -> Args {
    parse_args(&argv(items)).expect("参数应解析成功")
}

/// 解析并断言失败,返回错误信息
fn parse_err(items: &[&str]) -> String {
    parse_args(&argv(items)).expect_err("参数应解析失败")
}

// ---------------------------------------------------------------------------
// 正常路径:三参数齐全
// ---------------------------------------------------------------------------

/// 三个 slow-zone 参数齐全时,`slow_zone()` 返回 (start, end_exclusive, bps)
///
/// 关键契约:元组第二个元素是 **end_exclusive = start + len**,而非 len 本身,
/// 以对齐 `ThrottledServer::start_with_slow_zone` 的
/// `(zone_start, zone_end_exclusive, slow_bps)` 约定。
#[test]
fn test_baseline_args_slow_zone_all_three_present_returns_tuple() {
    let args = parse_ok(&[
        "--slow-zone-start",
        "4194304",
        "--slow-zone-len",
        "1048576",
        "--slow-zone-bps",
        "524288",
    ]);

    assert_eq!(args.slow_zone_start, Some(4_194_304));
    assert_eq!(args.slow_zone_len, Some(1_048_576));
    assert_eq!(args.slow_zone_bps, Some(524_288));
    assert_eq!(
        args.slow_zone(),
        Some((4_194_304, 5_242_880, 524_288)),
        "第二个元素应为 end_exclusive = start + len,而非 len"
    );
}

/// slow-zone 三参数走 parse_size,K/M/G(十进制)与 KiB/MiB/GiB(二进制)后缀均生效
#[test]
fn test_baseline_args_slow_zone_size_suffixes_parsed() {
    // 十进制后缀:M = 1000^2,K = 1000
    let decimal = parse_ok(&[
        "--slow-zone-start",
        "4M",
        "--slow-zone-len",
        "1M",
        "--slow-zone-bps",
        "512K",
    ]);
    assert_eq!(
        decimal.slow_zone(),
        Some((4_000_000, 5_000_000, 512_000)),
        "K/M 应按十进制(1000)换算"
    );

    // 二进制后缀:MiB = 1024^2;bps 用 G = 1000^3
    let binary = parse_ok(&[
        "--slow-zone-start",
        "4MiB",
        "--slow-zone-len",
        "1MiB",
        "--slow-zone-bps",
        "1G",
    ]);
    assert_eq!(
        binary.slow_zone(),
        Some((4_194_304, 5_242_880, 1_000_000_000)),
        "MiB 应按二进制(1024)换算,G 按十进制"
    );
}

/// 边界:zone_start = 0 是合法起点,不得被当作「未设置」
///
/// 排除用 `if start != 0` 之类真值判断代替 `Option` 判空的错误实现。
#[test]
fn test_baseline_args_slow_zone_zero_start_is_some_not_none() {
    let args = parse_ok(&[
        "--slow-zone-start",
        "0",
        "--slow-zone-len",
        "1M",
        "--slow-zone-bps",
        "512K",
    ]);

    assert_eq!(args.slow_zone_start, Some(0), "0 应被记录为 Some(0)");
    assert_eq!(
        args.slow_zone(),
        Some((0, 1_000_000, 512_000)),
        "zone_start = 0 应正常生效"
    );
}

// ---------------------------------------------------------------------------
// 缺一个:三种缺法各一,slow_zone() 必须为 None
// ---------------------------------------------------------------------------

/// 缺 --slow-zone-start → slow_zone() 为 None(另两个参数本身仍被正常记录)
#[test]
fn test_baseline_args_slow_zone_missing_start_returns_none() {
    let args = parse_ok(&["--slow-zone-len", "1M", "--slow-zone-bps", "512K"]);

    assert_eq!(args.slow_zone_start, None);
    assert_eq!(
        args.slow_zone_len,
        Some(1_000_000),
        "已给出的参数仍应被记录"
    );
    assert_eq!(args.slow_zone_bps, Some(512_000), "已给出的参数仍应被记录");
    assert_eq!(args.slow_zone(), None, "缺 start 时不得生效");
}

/// 缺 --slow-zone-len → slow_zone() 为 None
#[test]
fn test_baseline_args_slow_zone_missing_len_returns_none() {
    let args = parse_ok(&["--slow-zone-start", "4M", "--slow-zone-bps", "512K"]);

    assert_eq!(args.slow_zone_len, None);
    assert_eq!(
        args.slow_zone_start,
        Some(4_000_000),
        "已给出的参数仍应被记录"
    );
    assert_eq!(args.slow_zone_bps, Some(512_000), "已给出的参数仍应被记录");
    assert_eq!(args.slow_zone(), None, "缺 len 时不得生效");
}

/// 缺 --slow-zone-bps → slow_zone() 为 None
#[test]
fn test_baseline_args_slow_zone_missing_bps_returns_none() {
    let args = parse_ok(&["--slow-zone-start", "4M", "--slow-zone-len", "1M"]);

    assert_eq!(args.slow_zone_bps, None);
    assert_eq!(
        args.slow_zone_start,
        Some(4_000_000),
        "已给出的参数仍应被记录"
    );
    assert_eq!(
        args.slow_zone_len,
        Some(1_000_000),
        "已给出的参数仍应被记录"
    );
    assert_eq!(args.slow_zone(), None, "缺 bps 时不得生效");
}

// ---------------------------------------------------------------------------
// 错误输入
// ---------------------------------------------------------------------------

/// 数值格式错误 → parse_args 返回 Err(三个参数各测一次)
#[test]
fn test_baseline_args_slow_zone_invalid_number_returns_err() {
    let cases: [(&str, &str); 4] = [
        ("--slow-zone-start", "abc"),
        ("--slow-zone-len", "12x"),
        ("--slow-zone-bps", ""),
        ("--slow-zone-bps", "-5"),
    ];

    for (flag, value) in cases {
        let err = parse_err(&[flag, value]);
        assert!(
            !err.is_empty(),
            "{flag} {value} 应返回非空错误信息,实测为空"
        );
    }
}

/// 参数后缺少取值(flag 位于 argv 末尾)→ parse_args 返回 Err,而非 panic
#[test]
fn test_baseline_args_slow_zone_missing_value_after_flag_returns_err() {
    for flag in ["--slow-zone-start", "--slow-zone-len", "--slow-zone-bps"] {
        let err = parse_err(&[flag]);
        assert!(
            !err.is_empty(),
            "{flag} 缺少取值时应返回非空错误信息,实测为空"
        );
    }
}

// ---------------------------------------------------------------------------
// 回归保护:搬迁 + 新增字段不得改变既有解析行为
// ---------------------------------------------------------------------------

/// 不传任何 slow-zone 参数时,既有参数解析结果不变,且 slow_zone() 为 None
#[test]
fn test_baseline_args_without_slow_zone_keeps_existing_parsing() {
    let args = parse_ok(&[
        "--size",
        "128MiB",
        "--bps",
        "50M",
        "--runs",
        "5",
        "--rtt-ms",
        "25",
        "--concurrency",
        "32",
    ]);

    assert_eq!(args.size, 128 * 1024 * 1024, "--size 解析不变");
    assert_eq!(args.bps, 50_000_000, "--bps 解析不变");
    assert_eq!(args.runs, 5, "--runs 解析不变");
    assert_eq!(args.rtt_ms, 25, "--rtt-ms 解析不变");
    assert_eq!(args.concurrency, 32, "--concurrency 解析不变");

    assert_eq!(args.slow_zone_start, None);
    assert_eq!(args.slow_zone_len, None);
    assert_eq!(args.slow_zone_bps, None);
    assert_eq!(args.slow_zone(), None, "未配置慢区间时必须为 None");
}

/// 空参数表 → 全部落到既有默认值,且 slow_zone() 为 None
#[test]
fn test_baseline_args_empty_argv_keeps_existing_defaults() {
    let args = parse_ok(&[]);

    assert_eq!(args.size, 64 * 1024 * 1024, "默认 size 不变");
    assert_eq!(args.rtt_ms, 0, "默认 rtt_ms 不变");
    assert_eq!(args.bps, 0, "默认 bps 不变");
    assert_eq!(args.concurrency, 16, "默认 concurrency 不变");
    assert_eq!(args.runs, 3, "默认 runs 不变");
    assert_eq!(args.mirror_rtt_ms, 200, "默认 mirror_rtt_ms 不变");
    assert_eq!(args.mirror_bps, 5_000_000, "默认 mirror_bps 不变");
    assert!(args.url.is_none(), "默认 url 不变");
    assert!(args.mirrors.is_empty(), "默认 mirrors 不变");

    assert_eq!(args.slow_zone(), None, "默认不启用慢区间");
}

/// `parse_size` 搬迁后行为不变(K/M/G、KiB/MiB/GiB、无后缀、小数、错误)
///
/// slow-zone 三参数复用同一函数,这里直接锁定它,避免搬迁过程中被顺手改动。
#[test]
fn test_baseline_args_parse_size_behavior_unchanged() {
    assert_eq!(parse_size("1024"), Ok(1024), "无后缀按字节");
    assert_eq!(parse_size("1k"), Ok(1_000), "k = 1000");
    assert_eq!(parse_size("1M"), Ok(1_000_000), "M = 1000^2");
    assert_eq!(parse_size("1G"), Ok(1_000_000_000), "G = 1000^3");
    assert_eq!(parse_size("1KiB"), Ok(1024), "KiB = 1024");
    assert_eq!(parse_size("1MiB"), Ok(1024 * 1024), "MiB = 1024^2");
    assert_eq!(parse_size("1GiB"), Ok(1024 * 1024 * 1024), "GiB = 1024^3");
    assert_eq!(parse_size("0.5MiB"), Ok(512 * 1024), "支持小数");
    assert_eq!(parse_size(" 8M "), Ok(8_000_000), "两端空白应被裁剪");
    assert!(parse_size("abc").is_err(), "非数字应报错");
    assert!(parse_size("-1").is_err(), "负数应报错");
}

// ── P0-1 + P0-2 新增参数(rebalance_off / tls / loss_rate / mirror_*) ──────

/// `--rebalance-off` 默认 false,显式传入后为 true
#[test]
fn test_baseline_args_rebalance_off_default_false_and_flag_sets_true() {
    let a = parse_ok(&[]);
    assert!(!a.rebalance_off, "默认 false");

    let a = parse_ok(&["--rebalance-off"]);
    assert!(a.rebalance_off, "--rebalance-off 应置 true");
}

/// `--tls` / `--mirror-tls` 默认 false
#[test]
fn test_baseline_args_tls_defaults_false_and_flags_set_true() {
    let a = parse_ok(&[]);
    assert!(!a.tls, "默认 tls=false");
    assert!(!a.mirror_tls, "默认 mirror_tls=false");

    let a = parse_ok(&["--tls", "--mirror-tls"]);
    assert!(a.tls, "--tls 应置 true");
    assert!(a.mirror_tls, "--mirror-tls 应置 true");
}

/// `--loss-rate` 范围校验:[0.0, 1.0] 合法,超范围报错
#[test]
fn test_baseline_args_loss_rate_range_validation() {
    let a = parse_ok(&["--loss-rate", "0.0"]);
    assert_eq!(a.loss_rate, 0.0, "0.0 合法");

    let a = parse_ok(&["--loss-rate", "0.5"]);
    assert!((a.loss_rate - 0.5).abs() < f64::EPSILON, "0.5 合法");

    let a = parse_ok(&["--loss-rate", "1.0"]);
    assert!((a.loss_rate - 1.0).abs() < f64::EPSILON, "1.0 合法");

    assert!(
        parse_args(&["--loss-rate".to_string(), "1.5".to_string()]).is_err(),
        ">1.0 应报错"
    );
    assert!(
        parse_args(&["--loss-rate".to_string(), "-0.1".to_string()]).is_err(),
        "<0.0 应报错"
    );
    assert!(
        parse_args(&["--loss-rate".to_string(), "abc".to_string()]).is_err(),
        "非数字应报错"
    );
}

/// `--mirror-loss-rate` 独立解析,与主源互不影响
#[test]
fn test_baseline_args_mirror_loss_rate_independent_from_primary() {
    let a = parse_ok(&["--loss-rate", "0.1", "--mirror-loss-rate", "0.2"]);
    assert!((a.loss_rate - 0.1).abs() < 1e-9, "主源 0.1");
    assert!((a.mirror_loss_rate - 0.2).abs() < 1e-9, "镜像源 0.2");
}

/// `--mirror-slow-zone-*` 三参数齐全时 `mirror_slow_zone()` 返回元组
#[test]
fn test_baseline_args_mirror_slow_zone_all_three_present() {
    let a = parse_ok(&[
        "--mirror-slow-zone-start",
        "1MiB",
        "--mirror-slow-zone-len",
        "2MiB",
        "--mirror-slow-zone-bps",
        "1M",
    ]);
    assert_eq!(
        a.mirror_slow_zone(),
        Some((1024 * 1024, 3 * 1024 * 1024, 1_000_000)),
        "镜像慢区间三参数齐全应返回元组"
    );
}

/// `--mirror-slow-zone-*` 缺任一参数时 `mirror_slow_zone()` 返回 None
#[test]
fn test_baseline_args_mirror_slow_zone_missing_returns_none() {
    let a = parse_ok(&["--mirror-slow-zone-start", "1MiB"]);
    assert_eq!(a.mirror_slow_zone(), None, "缺 len/bps 应返回 None");
}

/// 新参数与既有参数共存,互不影响解析
#[test]
fn test_baseline_args_new_flags_do_not_break_existing_parsing() {
    let a = parse_ok(&[
        "--size",
        "128MiB",
        "--concurrency",
        "8",
        "--runs",
        "5",
        "--rebalance-off",
        "--tls",
        "--loss-rate",
        "0.05",
        "--local-mirror",
        "--mirror-tls",
        "--mirror-loss-rate",
        "0.1",
        "--mirror-slow-zone-start",
        "32MiB",
        "--mirror-slow-zone-len",
        "8MiB",
        "--mirror-slow-zone-bps",
        "5M",
    ]);
    assert_eq!(a.size, 128 * 1024 * 1024);
    assert_eq!(a.concurrency, 8);
    assert_eq!(a.runs, 5);
    assert!(a.rebalance_off);
    assert!(a.tls);
    assert!((a.loss_rate - 0.05).abs() < 1e-9);
    assert!(a.local_mirror);
    assert!(a.mirror_tls);
    assert!((a.mirror_loss_rate - 0.1).abs() < 1e-9);
    assert_eq!(
        a.mirror_slow_zone(),
        Some((32 * 1024 * 1024, 40 * 1024 * 1024, 5_000_000))
    );
}
