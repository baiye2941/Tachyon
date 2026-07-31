//! 代理并发推断逻辑
//!
//! 从 downloader.rs 拆分:根据代理类型(直连/本机/远程)推断并发上限。
//! 跨境 HTTP_PROXY 有共享出口瓶颈,需要冷启动 cap + 稳态天花板;
//! 本机 loopback 代理(Clash/v2rayN)与直连同等对待。

use super::*;

impl DownloadTask {
    /// 是否走系统/显式 HTTP 代理(direct/none 视为直连)。
    /// 含本机 loopback 代理;并发 cap 请用 [`Self::remote_http_proxy_active`]。
    #[allow(dead_code)] // 测试与诊断谓词;生产 cap 路径走 remote_http_proxy_active
    pub(super) fn http_proxy_active(&self) -> bool {
        self.resolved_http_proxy_url().is_some()
    }

    /// 解析当前生效的 HTTP 代理 URL(配置优先,否则环境变量)。
    /// `direct`/`none`/空串 → None。
    fn resolved_http_proxy_url(&self) -> Option<String> {
        if let Some(p) = &self.config.proxy {
            let t = p.trim();
            if t.eq_ignore_ascii_case("direct") || t.eq_ignore_ascii_case("none") {
                return None;
            }
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
        tachyon_core::config::resolve_http_proxy(None)
    }

    /// 代理 URL 是否指向本机(loopback)。
    ///
    /// 本机 Clash/v2rayN(`127.0.0.1:7897` 等)不是跨境共享代理瓶颈:
    /// 旧实现把「任意 HTTP_PROXY」一律 cap=2,导致国内网盘经本地代理时
    /// 并发永远跑不满(用户日志:total_frags=17 但 activeConcurrency 锁 2)。
    /// 无法解析 host 时保守视为非 loopback(仍套 cap)。
    pub(crate) fn is_loopback_proxy_url(proxy_url: &str) -> bool {
        if let Some(host) = Self::proxy_url_host(proxy_url) {
            return Self::host_is_loopback(&host);
        }
        false
    }

    /// 从代理 URL 提取 host(兼容 socks5 等非 WHATWG special scheme)。
    ///
    /// `url` crate 对 socks5 常把 authority 放进 path 而非 host;此处做兜底。
    pub(crate) fn proxy_url_host(proxy_url: &str) -> Option<String> {
        if let Ok(parsed) = url::Url::parse(proxy_url)
            && let Some(h) = parsed.host_str()
        {
            return Some(h.to_string());
        }
        // 兜底:scheme://[userinfo@]host[:port][/...]
        // socks5 等非 special scheme 时 url crate 常把 authority 放 path。
        let after_scheme = match proxy_url.split_once("://") {
            Some((_, rest)) => rest,
            None => proxy_url,
        };
        let authority = after_scheme.split('/').next().unwrap_or("");
        if authority.is_empty() {
            return None;
        }
        let host_port = match authority.rsplit_once('@') {
            Some((_, hp)) => hp,
            None => authority,
        };
        // IPv6 bracket form → 去掉括号再返回
        if let Some(rest) = host_port.strip_prefix('[') {
            let end = rest.find(']')?;
            return Some(rest[..end].to_string());
        }
        let host = match host_port.rsplit_once(':') {
            Some((h, port)) if !h.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h,
            _ => host_port,
        };
        let host = host.trim();
        if host.is_empty() {
            None
        } else {
            Some(host.to_string())
        }
    }

    pub(crate) fn host_is_loopback(host: &str) -> bool {
        let host = host.trim().trim_start_matches('[').trim_end_matches(']');
        if host.eq_ignore_ascii_case("localhost") || host.eq_ignore_ascii_case("localhost.") {
            return true;
        }
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            return ip.is_loopback();
        }
        false
    }

    /// 远程(非 loopback)HTTP 代理是否生效。
    ///
    /// 仅远程代理触发冷启动/稳态 cap、保守抬升步进;本机代理与直连同等对待并发。
    pub(super) fn remote_http_proxy_active(&self) -> bool {
        match self.resolved_http_proxy_url() {
            Some(url) => !Self::is_loopback_proxy_url(&url),
            None => false,
        }
    }

    /// 代理下片内 Range 窗口大小。
    ///
    /// 证据:跨境 HTTP_PROXY 约 35s 周期掐 TLS;8MiB 片在 ~600KB/s 下跑不完整片,
    /// EOF 后即使 partial resume 也丢当前连接窗口。2MiB 窗口把最坏重传上界从整片
    /// 收到 2MiB,且不改变 plan_fragments 边界(resume/rebalance 仍按分片 index)。
    /// 直连/本机代理返回 None(整片一次 Range,零额外请求开销)。
    pub(crate) fn proxy_range_window_bytes(&self) -> Option<u64> {
        const PROXY_RANGE_WINDOW: u64 = 2 * 1024 * 1024;
        if self.remote_http_proxy_active() {
            Some(PROXY_RANGE_WINDOW)
        } else {
            None
        }
    }

    /// 计算片内窗口结束偏移(含端):`min(start+window-1, frag_end)`。
    /// `window=None` 或 0 时返回 frag_end(整片)。
    pub(crate) fn range_window_end(start: u64, frag_end: u64, window: Option<u64>) -> u64 {
        match window {
            Some(w) if w > 0 => start.saturating_add(w.saturating_sub(1)).min(frag_end),
            _ => frag_end,
        }
    }

    /// 远程代理冷启动上限(低置信度):≤2。
    /// 本机代理(Clash loopback)不 cap——否则 17 片任务永远锁在 2。
    pub(super) fn proxy_cold_start_cap_for_config(&self, confidence: f64) -> Option<u32> {
        const PROXY_COLD_START_MAX: u32 = 2;
        const LOW_CONFIDENCE: f64 = 0.5;
        if confidence >= LOW_CONFIDENCE || !self.remote_http_proxy_active() {
            None
        } else {
            Some(PROXY_COLD_START_MAX)
        }
    }

    /// 远程代理稳态并发天花板(含 re-recommend 抬升)。
    ///
    /// 证据:经**跨境** HTTP_PROXY 的 kernel.org 同会话,c=2/c=4 健康时均 ~6MB/s;
    /// c=8 会爬到 5+ 打爆。c=2 已达吞吐, cap=4 只加倍连接面无 goodput 收益。
    /// 稳态 cap=2 与 soft-pressure floor、aria2 `-x2` 对齐;冷启动仍 ≤2。
    ///
    /// **本机 loopback 代理不套此 cap**:本地转发不是共享出口瓶颈,应允许
    /// 调度器按 max_concurrent_fragments 跑满(国内 CDN/网盘常见需求)。
    pub(super) fn proxy_steady_concurrency_ceiling(&self) -> Option<u32> {
        const PROXY_STEADY_MAX: u32 = 2;
        if self.remote_http_proxy_active() {
            Some(PROXY_STEADY_MAX)
        } else {
            None
        }
    }

    /// 对 desired 并发应用远程代理天花板(若有)。
    pub(super) fn apply_proxy_concurrency_ceiling(&self, desired: u32) -> u32 {
        match self.proxy_steady_concurrency_ceiling() {
            Some(cap) => desired.min(cap).max(1),
            None => desired.max(1),
        }
    }
}
