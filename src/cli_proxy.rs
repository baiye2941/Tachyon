//! CLI SOCKS 代理解析：参数 > 环境变量 > None，禁止硬编码默认代理。

/// 优先级：显式 CLI 参数 > 环境变量（与 `tachyon_core::config::detect_socks_proxy` 键序对齐）> None。
/// 键序：`ALL_PROXY > all_proxy > HTTPS_PROXY > https_proxy > HTTP_PROXY > http_proxy`。
/// 空字符串视为未设置。绝不发明 7897 等默认端口。
///
/// 注意：本函数只做“取第一个非空 URL”，不做 scheme 规范化（http→socks5）。
/// CLI 调试工具直接把结果交给 `MagnetConfig.socks_proxy_url`，scheme 由配置校验负责。
pub fn resolve_socks_proxy(
    cli: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Some(url) = cli {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    // 与 detect_socks_proxy 键序对齐：大写优先于小写（POSIX 大小写敏感）
    for key in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        if let Some(val) = env(key) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// 从 argv 解析可选 `--socks-proxy <url>`，不破坏 `<magnet> <out_dir>` 位置参数。
/// 返回 (magnet, out_dir, socks_proxy)。
pub fn parse_cli_args(args: &[String]) -> Result<(String, String, Option<String>), String> {
    if args.len() < 3 {
        return Err(format!(
            "用法: {} <magnet_url> <output_dir> [--socks-proxy <url>]",
            args.first().map(String::as_str).unwrap_or("tachyon")
        ));
    }
    let magnet = args[1].clone();
    let out_dir = args[2].clone();
    let mut socks: Option<String> = None;
    let mut i = 3;
    while i < args.len() {
        if args[i] == "--socks-proxy" {
            let url = args
                .get(i + 1)
                .ok_or_else(|| "--socks-proxy 需要一个 URL 参数".to_string())?;
            socks = Some(url.clone());
            i += 2;
        } else {
            return Err(format!("未知参数: {}", args[i]));
        }
    }
    Ok((magnet, out_dir, socks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_env(_k: &str) -> Option<String> {
        None
    }

    /// 测试注入：从 HashMap 读环境变量（仅测试用，避免生产 dead_code）。
    fn resolve_socks_proxy_from_map(
        cli: Option<&str>,
        env_map: &HashMap<String, String>,
    ) -> Option<String> {
        resolve_socks_proxy(cli, &|k| env_map.get(k).cloned())
    }

    #[test]
    fn test_cli_proxy_explicit_cli_wins() {
        let env = |k: &str| {
            if k == "ALL_PROXY" {
                Some("socks5://b:2".to_string())
            } else {
                None
            }
        };
        assert_eq!(
            resolve_socks_proxy(Some("socks5://a:1"), &env),
            Some("socks5://a:1".to_string())
        );
    }

    #[test]
    fn test_cli_proxy_env_all_proxy_when_no_cli() {
        let env = |k: &str| {
            if k == "ALL_PROXY" {
                Some("socks5://b:2".to_string())
            } else {
                None
            }
        };
        assert_eq!(
            resolve_socks_proxy(None, &env),
            Some("socks5://b:2".to_string())
        );
    }

    #[test]
    fn test_cli_proxy_none_when_no_cli_no_env() {
        let result = resolve_socks_proxy(None, &empty_env);
        assert_eq!(result, None, "无 CLI/env 时必须为 None，不得硬编码 7897");
        if let Some(ref s) = result {
            assert!(
                !s.contains("7897"),
                "不得硬编码 7897 代理，实际: {s}"
            );
        }
    }

    #[test]
    fn test_cli_proxy_empty_cli_falls_through_to_env() {
        let env = |k: &str| {
            if k == "HTTPS_PROXY" {
                Some("socks5://c:3".to_string())
            } else {
                None
            }
        };
        assert_eq!(
            resolve_socks_proxy(Some(""), &env),
            Some("socks5://c:3".to_string())
        );
        assert_eq!(
            resolve_socks_proxy(Some("   "), &env),
            Some("socks5://c:3".to_string())
        );
    }

    #[test]
    fn test_cli_proxy_env_priority_all_over_https_over_http() {
        let mut map = HashMap::new();
        map.insert("HTTP_PROXY".to_string(), "socks5://http:1".to_string());
        map.insert("HTTPS_PROXY".to_string(), "socks5://https:2".to_string());
        map.insert("ALL_PROXY".to_string(), "socks5://all:3".to_string());
        assert_eq!(
            resolve_socks_proxy_from_map(None, &map),
            Some("socks5://all:3".to_string())
        );

        map.remove("ALL_PROXY");
        assert_eq!(
            resolve_socks_proxy_from_map(None, &map),
            Some("socks5://https:2".to_string())
        );

        map.remove("HTTPS_PROXY");
        assert_eq!(
            resolve_socks_proxy_from_map(None, &map),
            Some("socks5://http:1".to_string())
        );
    }

    #[test]
    fn test_cli_proxy_env_lowercase_keys() {
        // 与 detect_socks_proxy 对齐：小写键在对应大写键缺失时生效
        let mut map = HashMap::new();
        map.insert("all_proxy".to_string(), "socks5://lower-all:1".to_string());
        assert_eq!(
            resolve_socks_proxy_from_map(None, &map),
            Some("socks5://lower-all:1".to_string())
        );

        // 大写优先于小写
        map.insert("ALL_PROXY".to_string(), "socks5://upper-all:2".to_string());
        assert_eq!(
            resolve_socks_proxy_from_map(None, &map),
            Some("socks5://upper-all:2".to_string())
        );

        map.clear();
        map.insert("https_proxy".to_string(), "socks5://lower-https:3".to_string());
        map.insert("http_proxy".to_string(), "socks5://lower-http:4".to_string());
        assert_eq!(
            resolve_socks_proxy_from_map(None, &map),
            Some("socks5://lower-https:3".to_string())
        );
    }

    #[test]
    fn test_cli_proxy_empty_env_values_treated_as_unset() {
        let env = |k: &str| match k {
            "ALL_PROXY" => Some(String::new()),
            "HTTPS_PROXY" => Some("   ".to_string()),
            "HTTP_PROXY" => Some("socks5://ok:9".to_string()),
            _ => None,
        };
        assert_eq!(
            resolve_socks_proxy(None, &env),
            Some("socks5://ok:9".to_string())
        );
    }

    #[test]
    fn test_cli_proxy_parse_args_basic() {
        let args = vec![
            "tachyon".into(),
            "magnet:?xt=urn:btih:abc".into(),
            "/tmp/out".into(),
        ];
        let (m, o, p) = parse_cli_args(&args).unwrap();
        assert_eq!(m, "magnet:?xt=urn:btih:abc");
        assert_eq!(o, "/tmp/out");
        assert_eq!(p, None);
    }

    #[test]
    fn test_cli_proxy_parse_args_with_socks() {
        let args = vec![
            "tachyon".into(),
            "magnet:?xt=urn:btih:abc".into(),
            "/tmp/out".into(),
            "--socks-proxy".into(),
            "socks5://127.0.0.1:1080".into(),
        ];
        let (m, o, p) = parse_cli_args(&args).unwrap();
        assert_eq!(m, "magnet:?xt=urn:btih:abc");
        assert_eq!(o, "/tmp/out");
        assert_eq!(p.as_deref(), Some("socks5://127.0.0.1:1080"));
    }

    #[test]
    fn test_cli_proxy_parse_args_missing_proxy_value() {
        let args = vec![
            "tachyon".into(),
            "magnet:?xt=urn:btih:abc".into(),
            "/tmp/out".into(),
            "--socks-proxy".into(),
        ];
        assert!(parse_cli_args(&args).is_err());
    }

    #[test]
    fn test_cli_proxy_parse_args_too_few() {
        let args = vec!["tachyon".into()];
        assert!(parse_cli_args(&args).is_err());
    }
}
