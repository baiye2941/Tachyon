use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::Url;

use crate::{AmdError, AmdResult};

pub fn validate_public_http_url(url: &Url) -> AmdResult<()> {
    match url.scheme() {
        "http" | "https" => {}
        scheme => return Err(AmdError::Config(format!("不支持的协议: {scheme}"))),
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(AmdError::Config("URL 中不允许包含用户名或密码".into()));
    }

    let host = url
        .host_str()
        .ok_or_else(|| AmdError::Config("URL 主机为空".into()))?;
    let normalized_host = host.trim_end_matches('.');
    if normalized_host.eq_ignore_ascii_case("localhost") {
        return Err(AmdError::Config("不允许访问 localhost".into()));
    }
    if let Ok(ip) = normalized_host.parse::<IpAddr>() {
        reject_forbidden_ip(ip)?;
    }

    Ok(())
}

pub fn reject_forbidden_ip(ip: IpAddr) -> AmdResult<()> {
    match ip {
        IpAddr::V4(v4) => reject_forbidden_ipv4(v4),
        IpAddr::V6(v6) => reject_forbidden_ipv6(v6),
    }
}

fn reject_forbidden_ipv4(ip: Ipv4Addr) -> AmdResult<()> {
    let octets = ip.octets();

    if ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip == Ipv4Addr::new(169, 254, 169, 254)
    {
        return Err(AmdError::Config(format!("不允许访问受限 IPv4 地址: {ip}")));
    }
    // 组播(224.0.0.0/4)和保留地址(240.0.0.0/4,含广播 255.255.255.255)
    if octets[0] >= 224 {
        return Err(AmdError::Config(format!("不允许访问受限 IPv4 地址: {ip}")));
    }
    // RFC 6598 Carrier-Grade NAT (100.64.0.0/10)
    if octets[0] == 100 && (octets[1] & 0xC0) == 0x40 {
        return Err(AmdError::Config(format!("不允许访问受限 IPv4 地址: {ip}")));
    }
    // RFC 5737 文档地址
    if ip == Ipv4Addr::new(192, 0, 2, 0)
        || ip == Ipv4Addr::new(198, 51, 100, 0)
        || ip == Ipv4Addr::new(203, 0, 113, 0)
    {
        return Err(AmdError::Config(format!("不允许访问受限 IPv4 地址: {ip}")));
    }
    Ok(())
}

fn reject_forbidden_ipv6(ip: Ipv6Addr) -> AmdResult<()> {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return reject_forbidden_ipv4(mapped);
    }

    let segments = ip.segments();
    let first_segment = segments[0];
    let unique_local = (first_segment & 0xfe00) == 0xfc00;
    let link_local = (first_segment & 0xffc0) == 0xfe80;
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || unique_local || link_local {
        return Err(AmdError::Config(format!("不允许访问受限 IPv6 地址: {ip}")));
    }
    // 站点本地地址 fec0::/10 (RFC 3879 已弃用但仍可能被解析)
    if (segments[0] & 0xFFC0) == 0xFEC0 {
        return Err(AmdError::Config(format!("不允许访问受限 IPv6 地址: {ip}")));
    }
    Ok(())
}

pub fn redact_url_for_log(url: &str) -> String {
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
    if basename.is_empty() {
        format!("{}://{}", parsed.scheme(), host)
    } else {
        format!("{}://{}/{}", parsed.scheme(), host, basename)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_credentials_in_url() {
        let url = Url::parse("https://user:secret@example.com/model.bin").unwrap();
        assert!(validate_public_http_url(&url).is_err());
    }

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

    #[test]
    fn rejects_multicast_and_broadcast_ipv4() {
        // 组播地址 (224.0.0.0/4)
        for ip in [
            Ipv4Addr::new(224, 0, 0, 1),
            Ipv4Addr::new(239, 255, 255, 250), // SSDP
            Ipv4Addr::new(240, 0, 0, 1),
            Ipv4Addr::new(255, 255, 255, 255), // 广播
        ] {
            assert!(
                reject_forbidden_ipv4(ip).is_err(),
                "{ip} should be rejected as multicast/broadcast"
            );
        }
    }

    #[test]
    fn rejects_cgnat_range() {
        // RFC 6598 Carrier-Grade NAT (100.64.0.0/10)
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
        // 100.63.255.255 不应被拦截(CGNAT 范围前)
        assert!(reject_forbidden_ipv4(Ipv4Addr::new(100, 63, 255, 255)).is_ok());
    }

    #[test]
    fn rejects_documentation_range() {
        // RFC 5737 文档地址
        for ip in [
            Ipv4Addr::new(192, 0, 2, 0),
            Ipv4Addr::new(198, 51, 100, 0),
            Ipv4Addr::new(203, 0, 113, 0),
        ] {
            assert!(
                reject_forbidden_ipv4(ip).is_err(),
                "{ip} should be rejected as documentation range"
            );
        }
    }

    #[test]
    fn rejects_ipv6_site_local() {
        // fec0::/10 (已弃用的站点本地地址)
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
}
