//! URL validation and safe HTTP helpers for omgb commands.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use url::Url;

const CLOUD_METADATA_HOSTS: &[&str] = &["metadata.google.internal", "169.254.169.254"];

fn normalize_host(raw: &str) -> String {
    let mut host = raw.to_ascii_lowercase();
    if host.starts_with('[') && host.ends_with(']') {
        host = host[1..host.len() - 1].to_string();
    } else {
        while host.ends_with('.') {
            host.pop();
        }
    }
    host
}

fn ipv4_in_cidr(ip: Ipv4Addr, base: [u8; 4], prefix: u8) -> bool {
    let ip = u32::from(ip);
    let base = u32::from(Ipv4Addr::from(base));
    let mask = if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    };
    (ip & mask) == (base & mask)
}

fn is_non_public_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ipv4_in_cidr(ip, [0, 0, 0, 0], 8)
        || ipv4_in_cidr(ip, [100, 64, 0, 0], 10)
        || ipv4_in_cidr(ip, [192, 0, 0, 0], 24)
        || ipv4_in_cidr(ip, [192, 0, 2, 0], 24)
        || ipv4_in_cidr(ip, [198, 18, 0, 0], 15)
        || ipv4_in_cidr(ip, [198, 51, 100, 0], 24)
        || ipv4_in_cidr(ip, [203, 0, 113, 0], 24)
        || ipv4_in_cidr(ip, [240, 0, 0, 0], 4)
}

fn is_non_public_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_non_public_ipv4(v4);
    }
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
}

fn is_loopback_addr(ip: IpAddr) -> bool {
    if ip.is_loopback() {
        return true;
    }
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
        IpAddr::V4(_) => false,
    }
}

fn is_non_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_non_public_ipv4(v4),
        IpAddr::V6(v6) => is_non_public_ipv6(v6),
    }
}

fn is_explicit_local_host(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(&host);
    let host = host.split('%').next().unwrap_or(host);
    if host == "localhost" {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Validate a URL for safe outbound use. When `allow_local` is true, explicit
/// loopback hosts (`localhost`, `127.0.0.0/8`, `::1`) are permitted; private
/// ranges and cloud metadata hosts remain blocked.
pub async fn validate_url(raw: &str, allow_local: bool) -> anyhow::Result<Url> {
    let url = Url::parse(raw).map_err(|e| anyhow::anyhow!("invalid URL: {e}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        anyhow::bail!("non-HTTP(S) protocol blocked: {}", url.scheme());
    }
    let host = normalize_host(url.host_str().unwrap_or(""));
    if host.is_empty() {
        anyhow::bail!("URL has no host");
    }
    if CLOUD_METADATA_HOSTS.contains(&host.as_str()) {
        anyhow::bail!("cloud metadata host blocked");
    }

    let explicit_local = is_explicit_local_host(&host);
    if allow_local && explicit_local {
        return Ok(url);
    }

    if explicit_local {
        anyhow::bail!("loopback host blocked; use --allow-local to enable");
    }

    let lookup = format!("{host}:80");
    let mut saw_addr = false;
    let mut addrs = tokio::net::lookup_host(lookup).await?;
    while let Some(addr) = addrs.next() {
        saw_addr = true;
        if is_non_public_ip(addr.ip()) {
            anyhow::bail!("host resolved to a private/non-public address");
        }
    }
    if !saw_addr {
        anyhow::bail!("host resolved to no addresses");
    }

    Ok(url)
}

/// Perform a GET request to a validated URL with an optional timeout.
pub async fn http_get_text(url: &Url, timeout: Duration) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(2))
        .build()?;
    let resp = client.get(url.as_str()).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "HTTP {} {}",
            resp.status().as_u16(),
            resp.status().canonical_reason().unwrap_or("")
        );
    }
    Ok(resp.text().await?)
}

/// Perform a JSON POST to a validated URL.
pub async fn http_post_json(
    url: &Url,
    headers: HashMap<String, String>,
    body: serde_json::Value,
    timeout: Duration,
) -> anyhow::Result<(u16, String)> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(2))
        .build()?;
    let mut req = client.post(url.as_str()).json(&body);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req.send().await?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, text))
}

pub fn default_bind_addr() -> SocketAddr {
    "127.0.0.1:2419".parse().expect("valid bind address")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_explicit_local_host() {
        assert!(is_explicit_local_host("localhost"));
        assert!(is_explicit_local_host("127.0.0.1"));
        assert!(is_explicit_local_host("::1"));
        assert!(!is_explicit_local_host("example.com"));
    }

    #[test]
    fn test_default_bind_addr() {
        assert_eq!(default_bind_addr().to_string(), "127.0.0.1:2419");
    }
}
