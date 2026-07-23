//! URL 安全校验与 SSRF 防护
//!
//! 提供多层 URL/IP 安全校验:
//! - `validate_public_http_url` — 公网 HTTP/HTTPS URL 校验
//! - `validate_resolved_ip` — DNS 解析后 IP 校验(防 DNS Rebinding)
//! - `validate_redirect` — 重定向目标逐跳校验
//! - `reject_forbidden_ip` — IP 地址黑名单校验
//! - `redact_url_for_log` — URL 日志脱敏(magnet 保留 info hash)
//! - `url_for_display` — UI 展示用 URL(magnet 等无 host scheme 原文放行)
//! - `url_identity_key` — URL 身份键(任务去重,magnet 按 info hash)
//! - `magnet_info_hash` — magnet URI info hash 提取

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

use url::Url;

use crate::{DownloadError, DownloadResult};

// ---------------------------------------------------------------------------
// URL 安全校验
// ---------------------------------------------------------------------------

pub fn validate_public_http_url(url: &Url) -> DownloadResult<()> {
    match url.scheme() {
        "http" | "https" => {}
        scheme => return Err(DownloadError::Config(format!("不支持的协议: {scheme}"))),
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(DownloadError::Config("URL 中不允许包含用户名或密码".into()));
    }

    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| DownloadError::Config("URL 主机为空".into()))?;
    // 去除尾部点后若为空(如 host 为 "." 或 ".."),视为退化主机并拒绝。
    // 此类 URL 可绕过 host_str() 为 None/"" 的检查,是 SSRF 防护的盲点。
    let normalized_host = host.trim_end_matches('.');
    if normalized_host.is_empty() {
        return Err(DownloadError::Config("URL 主机为空".into()));
    }
    if normalized_host.eq_ignore_ascii_case("localhost") {
        // test-harness feature 下放行 localhost:wiremock 等 mock HTTP server 绑定
        // 127.0.0.1,需绕过 loopback 拦截才能端到端测试 probe/download_range 等协议层
        // 路径。仅 dev-dependencies 开启此 feature,生产 binary 不受影响。
        #[cfg(not(feature = "test-harness"))]
        return Err(DownloadError::Config("不允许访问 localhost".into()));
    }
    if let Ok(ip) = normalized_host.parse::<IpAddr>() {
        reject_forbidden_ip(ip)?;
    }

    Ok(())
}

/// DNS 解析后校验:对 URL 主机执行 DNS 解析并检查每个解析出的 IP 地址
///
/// 防止 DNS Rebinding 攻击:攻击者可通过 DNS TTL=0 使首次解析返回公网 IP(通过校验),
/// 第二次解析返回内网 IP(如 169.254.169.254 云元数据服务)。
/// 此函数在 URL 字符串校验之后、发起连接之前调用,确保所有解析结果均为安全 IP。
///
/// # 返回值
///
/// 返回所有已验证的安全 IP 地址列表。**协议层必须使用这些 IP 进行连接**
/// (而非重新发起 DNS 查询),以消除 TOCTOU(Time-of-Check to Time-of-Use)窗口。
///
/// # 用法
///
/// ```ignore
/// let safe_ips = validate_resolved_ip(&url)?;
/// // 使用 safe_ips 中的 IP 直接建立连接,不再重新 DNS 解析
/// for ip in &safe_ips {
///     match connect_to(ip, port).await { ... }
/// }
/// ```
pub fn validate_resolved_ip(url: &Url) -> DownloadResult<Vec<IpAddr>> {
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| DownloadError::Config("URL 主机为空".into()))?;

    // 如果 host 已经是 IP 地址,直接校验即可(无需 DNS 解析)
    if let Ok(ip) = host.parse::<IpAddr>() {
        reject_forbidden_ip(ip)?;
        return Ok(vec![ip]);
    }

    // 对域名执行 DNS 解析
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|e| DownloadError::Network(format!("DNS 解析失败: {e}")))?;

    let mut resolved_ips = Vec::new();
    for addr in addrs {
        reject_forbidden_ip(addr.ip())?;
        resolved_ips.push(addr.ip());
    }

    if resolved_ips.is_empty() {
        return Err(DownloadError::Network("DNS 解析无结果".into()));
    }

    Ok(resolved_ips)
}

/// 重定向目标校验:对每次重定向的目标 URL 执行完整的 SSRF 校验
///
/// 防止攻击者通过合法公网 URL 通过初始校验后,通过服务端重定向(301/302/307/308)
/// 将请求导向内网地址。协议层应禁用 HTTP 客户端的自动重定向,改为手动跟随并在
/// 每一步调用此函数。
///
/// # 参数
///
/// - `redirect_url`: 重定向目标 URL
/// - `max_redirects`: 允许的最大重定向次数
/// - `current_redirect`: 当前已执行的重定向次数(从 0 开始)
pub fn validate_redirect(
    redirect_url: &Url,
    max_redirects: u32,
    current_redirect: u32,
) -> DownloadResult<Vec<IpAddr>> {
    if current_redirect >= max_redirects {
        return Err(DownloadError::Protocol(format!(
            "重定向次数超过上限 ({max_redirects})"
        )));
    }
    // 对每次重定向目标执行完整的 URL 校验 + DNS 解析校验
    validate_public_http_url(redirect_url)?;
    let safe_ips = validate_resolved_ip(redirect_url)?;
    Ok(safe_ips)
}

/// 判断 IP 是否属于受限 BT peer 地址范围。
///
/// 此分类不包含 `test-harness` 例外，供 BT 等不能访问本地网络的调用方复用。
pub fn is_restricted_peer_ip(ip: IpAddr) -> bool {
    classify_restricted_ip(ip).is_some()
}

#[derive(Clone, Copy)]
enum RestrictedIpKind {
    General,
    Documentation,
    Benchmark,
    IetfProtocolAssignments,
}

/// 受限地址规则的唯一来源；HTTP 和 BT 策略均复用此纯分类。
fn classify_restricted_ip(ip: IpAddr) -> Option<RestrictedIpKind> {
    match ip {
        IpAddr::V4(ip) => classify_restricted_ipv4(ip),
        IpAddr::V6(ip) => classify_restricted_ipv6(ip),
    }
}

fn classify_restricted_ipv4(ip: Ipv4Addr) -> Option<RestrictedIpKind> {
    let octets = ip.octets();

    if ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip == Ipv4Addr::new(169, 254, 169, 254)
        // 组播(224.0.0.0/4)和保留地址(240.0.0.0/4,含广播 255.255.255.255)
        || octets[0] >= 224
        // RFC 6598 Carrier-Grade NAT (100.64.0.0/10)
        || (octets[0] == 100 && (octets[1] & 0xC0) == 0x40)
    {
        return Some(RestrictedIpKind::General);
    }

    // RFC 5737 文档地址: 192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24
    // S-16: 匹配整个 /24 网段(前 3 个字节),而非仅 .0 网络地址
    let doc_ranges: [(u8, u8, u8); 3] = [(192, 0, 2), (198, 51, 100), (203, 0, 113)];
    if doc_ranges
        .iter()
        .any(|&(a, b, c)| octets[0] == a && octets[1] == b && octets[2] == c)
    {
        return Some(RestrictedIpKind::Documentation);
    }
    // RFC 2544 基准测试地址 (198.18.0.0/15)
    if octets[0] == 198 && (octets[1] == 18 || octets[1] == 19) {
        return Some(RestrictedIpKind::Benchmark);
    }
    // IETF Protocol Assignments (192.0.0.0/24)
    if octets[0] == 192 && octets[1] == 0 && octets[2] == 0 {
        return Some(RestrictedIpKind::IetfProtocolAssignments);
    }
    None
}

fn classify_restricted_ipv6(ip: Ipv6Addr) -> Option<RestrictedIpKind> {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return classify_restricted_ipv4(mapped);
    }

    let segments = ip.segments();
    let first_segment = segments[0];
    let unique_local = (first_segment & 0xfe00) == 0xfc00;
    let link_local = (first_segment & 0xffc0) == 0xfe80;
    // 站点本地地址 fec0::/10 (RFC 3879 已弃用但仍可能被解析)
    let site_local = (first_segment & 0xFFC0) == 0xFEC0;
    if ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || unique_local
        || link_local
        || site_local
    {
        Some(RestrictedIpKind::General)
    } else {
        None
    }
}

pub fn reject_forbidden_ip(ip: IpAddr) -> DownloadResult<()> {
    let Some(kind) = classify_restricted_ip(ip) else {
        return Ok(());
    };

    // 仅 HTTP 测试夹具允许 loopback 访问 wiremock；基础分类和 BT 策略不放宽。
    #[cfg(feature = "test-harness")]
    if is_loopback_ip(ip) {
        return Ok(());
    }

    Err(forbidden_ip_error(ip, kind))
}

#[cfg(feature = "test-harness")]
fn is_loopback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_loopback())
        }
    }
}

#[cfg(test)]
fn reject_forbidden_ipv4(ip: Ipv4Addr) -> DownloadResult<()> {
    reject_forbidden_ip(IpAddr::V4(ip))
}

fn forbidden_ip_error(ip: IpAddr, kind: RestrictedIpKind) -> DownloadError {
    match ip {
        IpAddr::V4(ip) => forbidden_ipv4_error(ip, kind),
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(|mapped| forbidden_ipv4_error(mapped, kind))
            .unwrap_or_else(|| DownloadError::Config(format!("不允许访问受限 IPv6 地址: {ip}"))),
    }
}

fn forbidden_ipv4_error(ip: Ipv4Addr, kind: RestrictedIpKind) -> DownloadError {
    let suffix = match kind {
        RestrictedIpKind::General => "",
        RestrictedIpKind::Documentation => " (RFC 5737 文档地址)",
        RestrictedIpKind::Benchmark => " (RFC 2544 基准测试地址)",
        RestrictedIpKind::IetfProtocolAssignments => " (IETF Protocol Assignments)",
    };
    DownloadError::Config(format!("不允许访问受限 IPv4 地址: {ip}{suffix}"))
}

/// 从 magnet URI 提取 info hash(xt=urn:btih:<hash>),统一小写。
///
/// info hash 是公开内容标识(与 dn=/tr= 等可能含敏感信息的参数不同),
/// 可用于日志标识与任务去重。非 magnet 链接或缺失 xt 参数时返回 None。
pub fn magnet_info_hash(url: &str) -> Option<String> {
    const MAGNET_PREFIX: &str = "magnet:?";
    const URN_BTIH_PREFIX: &str = "urn:btih:";
    let query = url
        .get(..MAGNET_PREFIX.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(MAGNET_PREFIX))
        .and_then(|_| url.get(MAGNET_PREFIX.len()..))?;
    for param in query.split('&') {
        let Some((name, value)) = param.split_once('=') else {
            continue;
        };
        if !name.eq_ignore_ascii_case("xt") {
            continue;
        }
        if let Some(hash) = value
            .get(..URN_BTIH_PREFIX.len())
            .filter(|urn| urn.eq_ignore_ascii_case(URN_BTIH_PREFIX))
            .and_then(|_| value.get(URN_BTIH_PREFIX.len()..))
        {
            let hash = hash.trim();
            if !hash.is_empty() {
                return Some(hash.to_lowercase());
            }
        }
    }
    None
}

pub fn redact_url_for_log(url: &str) -> String {
    // magnet 是 cannot-be-a-base scheme,host_str() 恒为 None,
    // 不能走通用 host 分支;info hash 是公开标识,保留供日志排查
    if let Some(hash) = magnet_info_hash(url) {
        return format!("magnet:xt=urn:btih:{hash}");
    }
    if url
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("magnet:"))
    {
        return "magnet:<no-info-hash>".to_string();
    }
    let Ok(parsed) = Url::parse(url) else {
        return "<invalid-url>".to_string();
    };
    let Some(host) = parsed.host_str() else {
        return "<invalid-url>".to_string();
    };
    let basename = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|segment| !segment.is_empty())
        .unwrap_or("");
    // 仅脱敏凭据、query、fragment,保留 scheme/host/basename 供日志排查
    if basename.is_empty() {
        format!("{}://{}", parsed.scheme(), host)
    } else {
        format!("{}://{}/{}", parsed.scheme(), host, basename)
    }
}

/// UI 展示用 URL:magnet/ed2k 等无 host 的内容寻址 scheme 原文放行
/// (无凭据概念,且前端需要可复制的真实链接);http(s) 仍剥 query/凭据(SEC-008)。
pub fn url_for_display(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else {
        // 无法解析时原样返回,不伪造占位符污染用户剪贴板
        return url.to_string();
    };
    if parsed.host_str().is_none() {
        return url.to_string();
    }
    redact_url_for_log(url)
}

/// URL 身份键(任务去重比较用):
/// - magnet 按 info hash —— 同一资源的不同 tracker/dn 参数判同,
///   不同资源不再因统一脱敏成占位符而误判重复;
/// - http(s) 按 scheme://host/basename —— 忽略签名 query 差异;
/// - 其余按原文。
pub fn url_identity_key(url: &str) -> String {
    if let Some(hash) = magnet_info_hash(url) {
        return format!("magnet:xt=urn:btih:{hash}");
    }
    let Ok(parsed) = Url::parse(url) else {
        return url.to_string();
    };
    if parsed.host_str().is_none() {
        return url.to_string();
    }
    redact_url_for_log(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_credentials_in_url() {
        let url = Url::parse("https://user:secret@example.com/model.bin").unwrap();
        assert!(validate_public_http_url(&url).is_err());
    }

    /// 验证 SSRF 防护拒绝 loopback/私网/metadata IP(生产模式)。
    ///
    /// test-harness feature 下 loopback(127.0.0.1/::1)被放行供 wiremock 使用,
    /// 此测试跳过。非 loopback 的受限 IP(私网/metadata)在 test-harness 下仍被拒绝,
    /// 由 `rejects_non_loopback_private_ips_under_test_harness` 覆盖。
    #[cfg(not(feature = "test-harness"))]
    #[test]
    fn rejects_private_and_metadata_ips() {
        for ip in [
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V6("fc00::1".parse().unwrap()),
            IpAddr::V6("fe80::1".parse().unwrap()),
            IpAddr::V6("::ffff:127.0.0.1".parse().unwrap()),
            IpAddr::V6("::ffff:10.0.0.1".parse().unwrap()),
        ] {
            assert!(reject_forbidden_ip(ip).is_err(), "{ip} should be rejected");
        }
    }

    /// test-harness 模式下验证非 loopback 受限 IP 仍被拒绝。
    ///
    /// loopback 放行后,私网/metadata/CGNAT 等仍应被拒绝,确保 test-harness
    /// 只放行 loopback 而非全部 SSRF 防护。
    #[cfg(feature = "test-harness")]
    #[test]
    fn rejects_non_loopback_private_ips_under_test_harness() {
        for ip in [
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
            IpAddr::V6("fc00::1".parse().unwrap()),
            IpAddr::V6("fe80::1".parse().unwrap()),
        ] {
            assert!(reject_forbidden_ip(ip).is_err(), "{ip} should be rejected");
        }
        // loopback 应被放行。
        assert!(reject_forbidden_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))).is_ok());
        assert!(reject_forbidden_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)).is_ok());
        assert!(reject_forbidden_ip("::ffff:127.0.0.1".parse().unwrap()).is_ok());
    }

    #[test]
    fn rejects_multicast_and_broadcast_ipv4() {
        for ip in [
            Ipv4Addr::new(224, 0, 0, 1),
            Ipv4Addr::new(239, 255, 255, 250),
            Ipv4Addr::new(240, 0, 0, 1),
            Ipv4Addr::new(255, 255, 255, 255),
        ] {
            assert!(
                reject_forbidden_ipv4(ip).is_err(),
                "{ip} should be rejected as multicast/broadcast"
            );
        }
    }

    #[test]
    fn rejects_cgnat_range() {
        for ip in [
            Ipv4Addr::new(100, 64, 0, 1),
            Ipv4Addr::new(100, 127, 255, 255),
            Ipv4Addr::new(100, 80, 0, 1),
        ] {
            assert!(
                reject_forbidden_ipv4(ip).is_err(),
                "{ip} should be rejected as CGNAT"
            );
        }
        assert!(reject_forbidden_ipv4(Ipv4Addr::new(100, 63, 255, 255)).is_ok());
    }

    #[test]
    fn rejects_documentation_range() {
        for ip in [
            Ipv4Addr::new(192, 0, 2, 0),
            Ipv4Addr::new(192, 0, 2, 1),
            Ipv4Addr::new(192, 0, 2, 255),
            Ipv4Addr::new(198, 51, 100, 0),
            Ipv4Addr::new(198, 51, 100, 42),
            Ipv4Addr::new(203, 0, 113, 0),
            Ipv4Addr::new(203, 0, 113, 200),
        ] {
            assert!(
                reject_forbidden_ipv4(ip).is_err(),
                "{ip} should be rejected as documentation range"
            );
        }
        assert!(reject_forbidden_ipv4(Ipv4Addr::new(192, 0, 3, 1)).is_ok());
        assert!(reject_forbidden_ipv4(Ipv4Addr::new(198, 51, 101, 1)).is_ok());
    }

    #[test]
    fn rejects_rfc2544_benchmark_and_ietf_protocol_assignment_ranges() {
        for ip in [
            Ipv4Addr::new(198, 18, 0, 0),
            Ipv4Addr::new(198, 18, 0, 1),
            Ipv4Addr::new(198, 18, 255, 255),
            Ipv4Addr::new(198, 19, 0, 0),
            Ipv4Addr::new(198, 19, 255, 255),
        ] {
            assert!(
                reject_forbidden_ipv4(ip).is_err(),
                "{ip} should be rejected as RFC 2544 benchmark range"
            );
        }
        for ip in [
            Ipv4Addr::new(192, 0, 0, 0),
            Ipv4Addr::new(192, 0, 0, 1),
            Ipv4Addr::new(192, 0, 0, 255),
        ] {
            assert!(
                reject_forbidden_ipv4(ip).is_err(),
                "{ip} should be rejected as IETF Protocol Assignments range"
            );
        }
        assert!(reject_forbidden_ipv4(Ipv4Addr::new(198, 17, 255, 255)).is_ok());
        assert!(reject_forbidden_ipv4(Ipv4Addr::new(198, 20, 0, 0)).is_ok());
        assert!(reject_forbidden_ipv4(Ipv4Addr::new(192, 0, 1, 0)).is_ok());
    }

    #[test]
    fn rejects_ipv6_site_local() {
        for ip in [
            Ipv6Addr::new(0xfec0, 0, 0, 0, 0, 0, 0, 1),
            Ipv6Addr::new(0xfeb0, 0, 0, 0, 0, 0, 0, 1),
            Ipv6Addr::new(0xfeff, 0, 0, 0, 0, 0, 0, 1),
        ] {
            let ip_addr = IpAddr::V6(ip);
            assert!(
                reject_forbidden_ip(ip_addr).is_err(),
                "{ip} should be rejected as site-local"
            );
        }
    }

    /// 验证 localhost(带尾部点)被拒绝(生产模式)。
    /// test-harness 下 localhost 放行,此测试跳过。
    #[cfg(not(feature = "test-harness"))]
    #[test]
    fn rejects_localhost_with_trailing_dot() {
        let url = Url::parse("http://localhost./admin").unwrap();
        assert!(validate_public_http_url(&url).is_err());
    }

    #[test]
    fn accepts_public_https_url() {
        let url = Url::parse("https://example.com/releases/app.zip").unwrap();
        assert!(validate_public_http_url(&url).is_ok());
    }

    #[test]
    fn restricted_peer_ip_rejects_all_restricted_ranges_under_test_harness() {
        // BT peer 策略不得继承 HTTP test-harness 对 loopback 的放行例外。
        let restricted_ips = [
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V6("fd00::1".parse().unwrap()),
            IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1)),
            IpAddr::V6("fe80::1".parse().unwrap()),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)),
            IpAddr::V6("ff02::1".parse().unwrap()),
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)),
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1)),
            IpAddr::V4(Ipv4Addr::new(198, 18, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 0, 0, 1)),
        ];

        for ip in restricted_ips {
            assert!(
                is_restricted_peer_ip(ip),
                "受限 BT peer 地址必须被拒绝: {ip}"
            );
        }

        for ip in [
            IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
            IpAddr::V6("2001:4860:4860::8888".parse().unwrap()),
        ] {
            assert!(
                !is_restricted_peer_ip(ip),
                "公网 BT peer 地址不得被拒绝: {ip}"
            );
        }
    }

    #[test]
    fn redacts_query_fragment_and_credentials() {
        let redacted = redact_url_for_log(
            "https://token:secret@example.com/path/model.bin?token=abc&signature=def#frag",
        );
        assert_eq!(redacted, "https://example.com/model.bin");
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("signature"));
        assert!(!redacted.contains("secret"));
    }

    #[test]
    fn redacts_invalid_url_to_placeholder() {
        assert_eq!(redact_url_for_log("not a url"), "<invalid-url>");
    }

    /// 验证 IP 字面量 localhost 被拒绝(生产模式)。
    /// test-harness 下 loopback 放行,此测试跳过。
    #[cfg(not(feature = "test-harness"))]
    #[test]
    fn validate_resolved_ip_rejects_ip_literal_localhost() {
        let url = Url::parse("http://127.0.0.1/file.bin").unwrap();
        assert!(validate_resolved_ip(&url).is_err());
    }

    #[test]
    fn validate_resolved_ip_rejects_ip_literal_private() {
        let url = Url::parse("http://10.0.0.1/file.bin").unwrap();
        assert!(validate_resolved_ip(&url).is_err());
    }

    #[test]
    fn validate_resolved_ip_accepts_public_ip_literal() {
        let url = Url::parse("https://93.184.216.34/file.bin").unwrap();
        assert!(validate_resolved_ip(&url).is_ok());
    }

    #[test]
    fn validate_resolved_ip_rejects_empty_host() {
        // data: scheme 属 cannot-be-a-base URL,host_str() 返回 None(url crate doctest 已证实),
        // 且无 host 字段。validate_resolved_ip 应返回 Err("URL 主机为空")。
        // 注:函数内 .filter(|h| !h.is_empty()) 同时覆盖 Some("") 情况(如 file:/// URL)。
        let url = Url::parse("data:text/plain,Stuff").unwrap();
        assert_eq!(url.host_str(), None, "前置条件:该 URL 无 host");

        let result = validate_resolved_ip(&url);
        assert!(result.is_err(), "无 host 的 URL 应被拒绝,实际: {result:?}");
        assert!(
            result.unwrap_err().to_string().contains("主机为空"),
            "错误信息应包含'主机为空'"
        );
    }

    #[test]
    fn validate_redirect_rejects_exceeded_limit() {
        let url = Url::parse("https://example.com/file.bin").unwrap();
        assert!(validate_redirect(&url, 10, 10).is_err());
        assert!(validate_redirect(&url, 5, 5).is_err());
    }

    /// 验证重定向到内网目标被拒绝(生产模式)。
    /// test-harness 下 loopback 放行,此测试跳过。
    #[cfg(not(feature = "test-harness"))]
    #[test]
    fn validate_redirect_rejects_internal_target() {
        let url = Url::parse("http://127.0.0.1/admin").unwrap();
        assert!(validate_redirect(&url, 10, 0).is_err());
    }

    #[test]
    fn validate_redirect_accepts_within_limit() {
        let url = Url::parse("https://example.com/file.bin").unwrap();
        let _ = validate_redirect(&url, 10, 0);
    }

    // -----------------------------------------------------------------------
    // P1: validate_public_http_url 协议 / 空主机 / IP 字面量
    // -----------------------------------------------------------------------

    #[test]
    fn validate_public_http_url_rejects_non_http_scheme() {
        let url = Url::parse("ftp://example.com/file.bin").unwrap();
        let result = validate_public_http_url(&url);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("不支持的协议"));
    }

    #[test]
    fn validate_public_http_url_rejects_empty_host() {
        let url = Url::parse("https://./path/file.bin").unwrap();
        let result = validate_public_http_url(&url);
        assert!(result.is_err(), "空主机应被拒绝");
        assert!(result.unwrap_err().to_string().contains("URL 主机为空"));
    }

    #[test]
    fn validate_public_http_url_handles_ip_literal() {
        let public = Url::parse("https://8.8.8.8/file.bin").unwrap();
        assert!(
            validate_public_http_url(&public).is_ok(),
            "公网 IP 字面量应被允许"
        );

        let private = Url::parse("http://192.168.1.1/file.bin").unwrap();
        let result = validate_public_http_url(&private);
        assert!(result.is_err(), "私有 IP 字面量应被拒绝");
        assert!(result.unwrap_err().to_string().contains("受限"));
    }

    // -----------------------------------------------------------------------
    // magnet_info_hash: info hash 提取
    // -----------------------------------------------------------------------

    /// base32 大写 info hash(用户实际链接)应被提取并统一小写。
    #[test]
    fn magnet_info_hash_extracts_base32_hash_lowercased() {
        let url = "magnet:?xt=urn:btih:6GJXEQ5SGFF7BWMQL74VTOAXZC36XSJG&dn=example";
        assert_eq!(
            magnet_info_hash(url),
            Some("6gjxeq5sgff7bwmql74vtoaxzc36xsjg".to_string())
        );
    }

    /// 40 位 hex 大写 info hash 应被提取并统一小写。
    #[test]
    fn magnet_info_hash_extracts_hex_hash_lowercased() {
        let url = "magnet:?xt=urn:btih:0123456789ABCDEF0123456789ABCDEF01234567";
        assert_eq!(
            magnet_info_hash(url),
            Some("0123456789abcdef0123456789abcdef01234567".to_string())
        );
    }

    /// scheme、参数名、URN 前缀的大小写均不敏感。
    #[test]
    fn magnet_info_hash_ignores_scheme_and_param_name_case() {
        let url = "MAGNET:?Xt=URN:BTIH:6GJXEQ5SGFF7BWMQL74VTOAXZC36XSJG";
        assert_eq!(
            magnet_info_hash(url),
            Some("6gjxeq5sgff7bwmql74vtoaxzc36xsjg".to_string())
        );
    }

    /// xt 不在首位、与 dn=/tr= 及无值参数混杂时仍能正确定位。
    #[test]
    fn magnet_info_hash_finds_xt_among_other_params() {
        let url = "magnet:?dn=Some%20File&flag&tr=udp://tracker.example:1337/announce&xt=urn:btih:6GJXEQ5SGFF7BWMQL74VTOAXZC36XSJG&tr=udp://tracker2.example:6969";
        assert_eq!(
            magnet_info_hash(url),
            Some("6gjxeq5sgff7bwmql74vtoaxzc36xsjg".to_string())
        );
    }

    #[test]
    fn magnet_info_hash_returns_none_without_xt() {
        assert!(
            magnet_info_hash("magnet:?dn=only-name").is_none(),
            "缺 xt 参数应返回 None"
        );
    }

    /// xt 指向非 btih 的 URN(如 sha1)不是 info hash。
    #[test]
    fn magnet_info_hash_returns_none_for_non_btih_xt() {
        assert!(
            magnet_info_hash("magnet:?xt=urn:sha1:6GJXEQ5SGFF7BWMQL74VTOAXZC36XSJG").is_none(),
            "非 btih 的 xt 应返回 None"
        );
    }

    #[test]
    fn magnet_info_hash_returns_none_for_non_magnet_url() {
        assert!(
            magnet_info_hash("https://example.com/file.bin").is_none(),
            "非 magnet 链接应返回 None"
        );
    }

    /// 空 hash(含纯空白)不应被接受。
    #[test]
    fn magnet_info_hash_returns_none_for_empty_hash() {
        assert!(
            magnet_info_hash("magnet:?xt=urn:btih:").is_none(),
            "空 hash 应返回 None"
        );
        assert!(
            magnet_info_hash("magnet:?xt=urn:btih:   ").is_none(),
            "纯空白 hash 应返回 None"
        );
    }

    // -----------------------------------------------------------------------
    // redact_url_for_log: magnet 分支
    // -----------------------------------------------------------------------

    /// 完整 magnet 脱敏后仅保留小写 info hash,dn=/tr= 内容不得进入日志。
    #[test]
    fn redacts_magnet_to_info_hash_only() {
        let redacted = redact_url_for_log(
            "magnet:?xt=urn:btih:6GJXEQ5SGFF7BWMQL74VTOAXZC36XSJG&dn=Secret%20Name&tr=udp://tracker.example:1337/announce",
        );
        assert_eq!(
            redacted,
            "magnet:xt=urn:btih:6gjxeq5sgff7bwmql74vtoaxzc36xsjg"
        );
        assert!(!redacted.contains("Secret"), "dn 内容不得出现在日志中");
        assert!(
            !redacted.contains("tracker.example"),
            "tr 内容不得出现在日志中"
        );
    }

    /// 无 xt 的 magnet 无法提取 info hash,统一脱敏为占位符。
    #[test]
    fn redacts_magnet_without_xt_to_placeholder() {
        assert_eq!(redact_url_for_log("magnet:?dn=x"), "magnet:<no-info-hash>");
    }

    // -----------------------------------------------------------------------
    // url_for_display: UI 展示
    // -----------------------------------------------------------------------

    /// magnet 无 host 与凭据概念,前端需要可复制的完整链接,原文放行。
    #[test]
    fn url_for_display_passes_magnet_through_verbatim() {
        let magnet = "magnet:?xt=urn:btih:6GJXEQ5SGFF7BWMQL74VTOAXZC36XSJG&dn=Some%20File&tr=udp://tracker.example:1337/announce";
        assert_eq!(url_for_display(magnet), magnet);
    }

    /// http(s) 仍剥 query/fragment/凭据(SEC-008)。
    #[test]
    fn url_for_display_strips_http_query_and_credentials() {
        let display = url_for_display(
            "https://user:secret@example.com/path/model.bin?token=abc&signature=def#frag",
        );
        assert_eq!(display, "https://example.com/model.bin");
        assert!(!display.contains("abc"));
        assert!(!display.contains("secret"));
    }

    /// 无法解析的字符串原样返回,不伪造占位符污染用户剪贴板。
    #[test]
    fn url_for_display_returns_unparseable_input_verbatim() {
        assert_eq!(url_for_display("not a url"), "not a url");
    }

    // -----------------------------------------------------------------------
    // url_identity_key: 任务去重身份键
    // -----------------------------------------------------------------------

    /// 同一资源(info hash 相同)的 magnet,tr=/dn= 不同、hash 大小写不同仍判同。
    #[test]
    fn url_identity_key_groups_magnets_by_info_hash() {
        let a = "magnet:?xt=urn:btih:6GJXEQ5SGFF7BWMQL74VTOAXZC36XSJG&dn=File%20A&tr=udp://tracker1.example:1337/announce";
        let b = "magnet:?dn=File%20B&tr=udp://tracker2.example:6969/announce&xt=urn:btih:6gjxeq5sgff7bwmql74vtoaxzc36xsjg";
        assert_eq!(url_identity_key(a), url_identity_key(b));

        let c = "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=File%20A";
        assert_ne!(url_identity_key(a), url_identity_key(c));
    }

    /// http(s) 按 scheme://host/basename 判同,忽略签名 query 差异。
    #[test]
    fn url_identity_key_ignores_http_query_differences() {
        let a = "https://example.com/path/model.bin?token=abc";
        let b = "https://example.com/path/model.bin?token=xyz&signature=123";
        assert_eq!(url_identity_key(a), url_identity_key(b));

        let c = "https://example.com/path/other.bin?token=abc";
        assert_ne!(url_identity_key(a), url_identity_key(c));
    }

    /// 无法解析的字符串按原文作为身份键。
    #[test]
    fn url_identity_key_returns_unparseable_input_verbatim() {
        assert_eq!(url_identity_key("not a url"), "not a url");
        // 不同的不可解析字符串不再塌缩为同一占位符而误判重复
        assert_ne!(
            url_identity_key("not a url"),
            url_identity_key("also not a url")
        );
    }

    /// 无 xt 的 magnet 无 info hash 可提取,host_str() 为 None,按原文返回。
    #[test]
    fn url_identity_key_returns_magnet_without_xt_verbatim() {
        let url = "magnet:?dn=only-name";
        assert_eq!(url_identity_key(url), url);
    }
}
