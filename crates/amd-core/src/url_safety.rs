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
    if ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip == Ipv4Addr::new(169, 254, 169, 254)
    {
        return Err(AmdError::Config(format!("不允许访问受限 IPv4 地址: {ip}")));
    }
    Ok(())
}

fn reject_forbidden_ipv6(ip: Ipv6Addr) -> AmdResult<()> {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return reject_forbidden_ipv4(mapped);
    }

    let first_segment = ip.segments()[0];
    let unique_local = (first_segment & 0xfe00) == 0xfc00;
    let link_local = (first_segment & 0xffc0) == 0xfe80;
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || unique_local || link_local {
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
