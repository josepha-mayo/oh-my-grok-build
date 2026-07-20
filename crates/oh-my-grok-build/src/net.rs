//! URL validation and safe HTTP helpers for omgb commands.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use url::Url;

const CLOUD_METADATA_HOSTS: &[&str] = &[
    "metadata.google.internal",
    "metadata",
    "169.254.169.254",
    "fd00:ec2::254",
    "168.63.129.16",
    "100.100.100.200",
];

fn local_hostname() -> Option<String> {
    Some(
        gethostname::gethostname()
            .to_string_lossy()
            .to_ascii_lowercase(),
    )
    .filter(|h| !h.is_empty())
}

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

fn is_non_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_non_public_ipv4(v4),
        IpAddr::V6(v6) => is_non_public_ipv6(v6),
    }
}

/// True for RFC1918 / RFC4193 private ranges (plus RFC6598 CGN shared
/// space) that a user may legitimately want to reach on a local network.
fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || ipv4_in_cidr(v4, [100, 64, 0, 0], 10),
        IpAddr::V6(v6) => v6.is_unique_local(),
    }
}

/// True when every resolved address is loopback or (when allowed) private.
/// Used to reject plaintext http/ws to public destinations.
fn all_addrs_loopback_or_private(addrs: &[SocketAddr], allow_private: bool) -> bool {
    addrs
        .iter()
        .all(|a| a.ip().is_loopback() || (allow_private && is_private_ip(a.ip())))
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

fn lookup_port(url: &Url) -> u16 {
    url.port().unwrap_or_else(|| match url.scheme() {
        "https" | "wss" => 443,
        _ => 80,
    })
}

/// A URL whose destination has been validated and pinned to a set of addresses.
pub struct ValidatedUrl {
    pub url: Url,
    pub addrs: Vec<SocketAddr>,
}

fn validate_host_name(host: &str) -> anyhow::Result<()> {
    if host.is_empty() {
        anyhow::bail!("URL has no host");
    }
    if CLOUD_METADATA_HOSTS.contains(&host) {
        anyhow::bail!("cloud metadata host blocked");
    }
    if local_hostname().as_deref() == Some(host) {
        anyhow::bail!("local machine hostname blocked");
    }
    Ok(())
}

fn reject_userinfo(url: &Url) -> anyhow::Result<()> {
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("URLs with embedded credentials are not allowed");
    }
    Ok(())
}

/// Format `host:port` for `tokio::net::lookup_host`, bracketing IPv6 addresses.
/// IPv6 zone identifiers are stripped for lookup; `::1` does not need a zone,
/// and other scoped addresses are rejected as private by the caller.
fn lookup_addr(host: &str, port: u16) -> String {
    let ip = host.split('%').next().unwrap_or(host);
    if ip.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv6()) {
        format!("[{ip}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

async fn resolve_host(
    host: &str,
    port: u16,
    allow_local: bool,
    allow_private: bool,
) -> anyhow::Result<Vec<SocketAddr>> {
    let explicit_local = is_explicit_local_host(host);

    if allow_local && explicit_local {
        let mut addrs = Vec::new();
        for addr in tokio::net::lookup_host(lookup_addr(host, port)).await? {
            if !addr.ip().is_loopback() {
                anyhow::bail!("loopback host resolved to a non-loopback address");
            }
            addrs.push(SocketAddr::new(addr.ip(), port));
        }
        if addrs.is_empty() {
            anyhow::bail!("loopback host resolved to no addresses");
        }
        return Ok(addrs);
    }

    if explicit_local {
        anyhow::bail!("loopback host blocked; use --allow-local to enable");
    }

    let mut saw_addr = false;
    let mut addrs = Vec::new();
    for addr in tokio::net::lookup_host(lookup_addr(host, port)).await? {
        saw_addr = true;
        if addr.ip().is_loopback() {
            anyhow::bail!("host resolved to a loopback address");
        }
        if is_non_public_ip(addr.ip()) && !(allow_private && is_private_ip(addr.ip())) {
            anyhow::bail!("host resolved to a private/non-public address");
        }
        addrs.push(SocketAddr::new(addr.ip(), port));
    }
    if !saw_addr {
        anyhow::bail!("host resolved to no addresses");
    }
    Ok(addrs)
}

/// Validate a URL for safe outbound use. When `allow_local` is true, explicit
/// loopback hosts (`localhost`, `127.0.0.0/8`, `::1`) are permitted. When
/// `allow_private` is true, RFC1918 / RFC4193 private addresses are also
/// permitted. Cloud metadata hosts remain blocked.
pub async fn validate_url(
    raw: &str,
    allow_local: bool,
    allow_private: bool,
) -> anyhow::Result<ValidatedUrl> {
    let url = Url::parse(raw).map_err(|e| anyhow::anyhow!("invalid URL: {e}"))?;
    reject_userinfo(&url)?;
    if url.scheme() != "http" && url.scheme() != "https" {
        anyhow::bail!("non-HTTP(S) protocol blocked: {}", url.scheme());
    }
    let host = normalize_host(url.host_str().unwrap_or(""));
    validate_host_name(&host)?;
    let port = lookup_port(&url);
    let addrs = resolve_host(&host, port, allow_local, allow_private).await?;
    if url.scheme() == "http" && !all_addrs_loopback_or_private(&addrs, allow_private) {
        anyhow::bail!("insecure HTTP to a public host; use https");
    }
    Ok(ValidatedUrl { url, addrs })
}

/// Returns true when the host of `raw` is an explicit loopback host
/// (`localhost`, `127.0.0.0/8`, `::1`).
pub fn is_url_host_loopback(raw: &str) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    is_explicit_local_host(&normalize_host(host))
}

/// Returns true when the host of `raw` resolves to a private/LAN address
/// (e.g. `192.168.x.x`, `10.x.x.x`, `fc00::/7`). This includes hostnames that
/// resolve to private IPs, not just literal IP addresses.
pub async fn is_url_host_private(raw: &str) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    let host = normalize_host(url.host_str().unwrap_or(""));
    if validate_host_name(&host).is_err() {
        return false;
    }
    let port = lookup_port(&url);
    resolve_host(&host, port, true, true)
        .await
        .is_ok_and(|addrs| addrs.iter().any(|a| is_private_ip(a.ip())))
}

/// Open a WebSocket/WebSocket-over-TLS connection to `raw`, validating the
/// host, scheme, and resolved addresses first. The stream is connected to the
/// resolved `SocketAddr`s so the destination cannot be re-resolved to a
/// private/cloud-metadata address after validation. Explicit loopback hosts
/// are always permitted; set `allow_private` to also allow LAN/private hosts.
pub async fn connect_ws_url(
    raw: &str,
    allow_private: bool,
    auth: Option<&str>,
) -> anyhow::Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    let url = Url::parse(raw).map_err(|e| anyhow::anyhow!("invalid URL: {e}"))?;
    reject_userinfo(&url)?;
    let scheme = url.scheme();
    if !matches!(scheme, "ws" | "wss") {
        anyhow::bail!("URL scheme must be ws or wss: {scheme}");
    }
    let host = normalize_host(url.host_str().unwrap_or(""));
    validate_host_name(&host)?;
    let port = lookup_port(&url);
    let addrs = resolve_host(&host, port, true, allow_private).await?;
    if scheme == "ws" && !all_addrs_loopback_or_private(&addrs, allow_private) {
        anyhow::bail!("insecure WebSocket to a public host; use wss");
    }

    let display_host = if host
        .split('%')
        .next()
        .unwrap_or(host.as_str())
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_ipv6())
    {
        format!("[{}]", host.split('%').next().unwrap_or(host.as_str()))
    } else {
        host.to_string()
    };

    let request = if let Some(token) = auth {
        http::Request::builder()
            .uri(raw)
            .header("Authorization", format!("Bearer {token}"))
            .body(())
            .map_err(|e| anyhow::anyhow!("invalid websocket request: {e}"))?
    } else {
        http::Request::builder()
            .uri(raw)
            .body(())
            .map_err(|e| anyhow::anyhow!("invalid websocket request: {e}"))?
    };

    let mut last_err = None;
    for addr in addrs {
        match tokio::net::TcpStream::connect(addr).await {
            Ok(stream) => {
                match tokio_tungstenite::client_async_tls(request.clone(), stream).await {
                    Ok((ws, _)) => return Ok(ws),
                    Err(e) => last_err = Some(format!("{addr}: handshake {e}")),
                }
            }
            Err(e) => last_err = Some(format!("{addr}: connect {e}")),
        }
    }
    anyhow::bail!(
        "failed to connect to {display_host}:{port}: {}",
        last_err.as_deref().unwrap_or("unknown")
    )
}

fn build_client(vurl: &ValidatedUrl, timeout: Duration) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none());
    if let Some(host) = vurl.url.host_str() {
        let host = host.to_ascii_lowercase();
        if !vurl.addrs.is_empty() {
            builder = builder.resolve_to_addrs(&host, &vurl.addrs);
            // reqwest's internal hyper URI keeps IPv6 addresses in brackets,
            // so register the bracketed form as well to guarantee the override
            // is applied for IPv6 URLs. Strip any zone identifier first.
            let ip_host = host.split('%').next().unwrap_or(&host);
            if ip_host.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv6()) {
                builder = builder.resolve_to_addrs(&format!("[{ip_host}]"), &vurl.addrs);
            }
        }
    }
    Ok(builder.build()?)
}

/// Perform a GET request to a validated URL with optional headers and timeout.
pub async fn http_get_text(
    vurl: &ValidatedUrl,
    headers: Option<&HashMap<String, String>>,
    timeout: Duration,
) -> anyhow::Result<String> {
    let client = build_client(vurl, timeout)?;
    let mut req = client.get(vurl.url.as_str());
    if let Some(h) = headers {
        for (k, v) in h {
            req = req.header(k.as_str(), v.as_str());
        }
    }
    let resp = req.send().await?;
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
    vurl: &ValidatedUrl,
    headers: &HashMap<String, String>,
    body: serde_json::Value,
    timeout: Duration,
) -> anyhow::Result<(u16, String)> {
    let client = build_client(vurl, timeout)?;
    let mut req = client.post(vurl.url.as_str()).json(&body);
    for (k, v) in headers.iter() {
        req = req.header(k.as_str(), v.as_str());
    }
    let resp = req.send().await?;
    let status = resp.status().as_u16();
    let text = resp.text().await?;
    Ok((status, text))
}

#[allow(dead_code)]
pub const DEFAULT_BIND_ADDR: SocketAddr =
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 2419));

#[allow(dead_code)]
pub fn default_bind_addr() -> SocketAddr {
    DEFAULT_BIND_ADDR
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
    fn test_validate_host_name_blocks_metadata() {
        assert!(validate_host_name("metadata.google.internal").is_err());
        assert!(validate_host_name("169.254.169.254").is_err());
        assert!(validate_host_name("example.com").is_ok());
    }

    #[test]
    fn test_ipv4_in_cidr() {
        assert!(ipv4_in_cidr("10.0.0.5".parse().unwrap(), [10, 0, 0, 0], 8));
        assert!(!ipv4_in_cidr(
            "172.16.0.5".parse().unwrap(),
            [10, 0, 0, 0],
            8
        ));
        assert!(ipv4_in_cidr(
            "192.168.1.1".parse().unwrap(),
            [192, 168, 0, 0],
            16
        ));
    }

    #[test]
    fn test_is_private_ip() {
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("192.168.1.1".parse().unwrap()));
        assert!(is_private_ip("100.64.0.1".parse().unwrap()));
        assert!(is_private_ip("fd00::1".parse().unwrap()));
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn test_is_non_public_ip() {
        assert!(is_non_public_ip("127.0.0.1".parse().unwrap()));
        assert!(is_non_public_ip("169.254.1.1".parse().unwrap()));
        assert!(is_non_public_ip("::1".parse().unwrap()));
        assert!(is_non_public_ip("fe80::1".parse().unwrap()));
        assert!(!is_non_public_ip("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn test_all_addrs_loopback_or_private() {
        let local = SocketAddr::new("127.0.0.1".parse().unwrap(), 80);
        let private = SocketAddr::new("192.168.1.1".parse().unwrap(), 80);
        let public = SocketAddr::new("8.8.8.8".parse().unwrap(), 80);
        assert!(all_addrs_loopback_or_private(&[local], false));
        assert!(all_addrs_loopback_or_private(&[private], true));
        assert!(!all_addrs_loopback_or_private(&[private], false));
        assert!(!all_addrs_loopback_or_private(&[public], false));
        assert!(!all_addrs_loopback_or_private(&[local, public], false));
    }

    #[test]
    fn test_normalize_host() {
        assert_eq!(normalize_host("Example.COM"), "example.com");
        assert_eq!(normalize_host("[::1]"), "::1");
        assert_eq!(normalize_host("host."), "host");
    }

    #[test]
    fn test_lookup_port() {
        assert_eq!(lookup_port(&Url::parse("http://example.com").unwrap()), 80);
        assert_eq!(
            lookup_port(&Url::parse("https://example.com").unwrap()),
            443
        );
        assert_eq!(lookup_port(&Url::parse("ws://example.com").unwrap()), 80);
        assert_eq!(lookup_port(&Url::parse("wss://example.com").unwrap()), 443);
        assert_eq!(
            lookup_port(&Url::parse("http://example.com:8080").unwrap()),
            8080
        );
    }

    #[test]
    fn test_default_bind_addr() {
        assert_eq!(default_bind_addr().to_string(), "127.0.0.1:2419");
    }
}
