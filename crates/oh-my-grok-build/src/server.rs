//! ACP/WebSocket relay helpers for `omgb serve` and `omgb connect`.
//!
//! `serve` runs a reverse proxy in front of the upstream Grok Build agent
//! server. The proxy adds origin checking, per-IP rate limiting, and
//! constant-time secret verification without modifying the upstream crate.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axum::{
    Router,
    extract::{
        ConnectInfo, Query, State, ws::CloseFrame, ws::Message, ws::WebSocket, ws::WebSocketUpgrade,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as UpstreamCloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode as UpstreamCloseCode;
use tracing::{info, warn};
use url::Url;

use crate::args::{ConnectArgs, ServeArgs};

const RATE_LIMIT_CLEANUP_INTERVAL_SECS: u64 = 60;
const MAX_TRACKED_IPS: usize = 4096;
const UPSTREAM_PORT_ATTEMPTS: usize = 20;

fn omg_dir() -> anyhow::Result<std::path::PathBuf> {
    crate::providers::omg_dir()
}

fn generate_secret() -> String {
    // Use the full UUIDv4 hex string (32 chars, 128 bits) for the pairing secret.
    uuid::Uuid::new_v4().to_string().replace('-', "")
}

fn is_link_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_unicast_link_local(),
    }
}

fn format_ip_for_url(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    }
}

fn local_non_loopback_ip() -> Option<IpAddr> {
    let hostname = gethostname::gethostname().into_string().ok()?;
    format!("{hostname}:0")
        .as_str()
        .to_socket_addrs()
        .ok()?
        .map(|a| a.ip())
        .find(|ip| !ip.is_loopback() && !ip.is_unspecified() && !is_link_local(ip))
}

fn pairing_host(bind_addr: SocketAddr, advertise_host: Option<IpAddr>) -> String {
    if let Some(ip) = advertise_host {
        return format_ip_for_url(ip);
    }
    match bind_addr.ip() {
        // Binding 0.0.0.0 means the server is listening on IPv4 only, so only an
        // IPv4 non-loopback address is useful in the pairing URL.
        IpAddr::V4(ip) if ip.is_unspecified() => match local_non_loopback_ip() {
            Some(ip @ IpAddr::V4(_)) => ip.to_string(),
            _ => "127.0.0.1".to_string(),
        },
        IpAddr::V4(ip) => ip.to_string(),
        // Binding :: listens on IPv6 (and usually IPv4 too), so any usable
        // non-loopback address works; IPv6 is bracketed.
        IpAddr::V6(ip) if ip.is_unspecified() => local_non_loopback_ip()
            .map(format_ip_for_url)
            .unwrap_or_else(|| "[::1]".to_string()),
        IpAddr::V6(ip) => format!("[{ip}]"),
    }
}

fn pairing_url(bind_addr: SocketAddr, advertise_host: Option<IpAddr>) -> String {
    let host = pairing_host(bind_addr, advertise_host);
    let is_loopback = advertise_host
        .map(|ip| ip.is_loopback())
        .unwrap_or_else(|| bind_addr.ip().is_loopback());
    let scheme = if is_loopback { "ws" } else { "wss" };
    let port = bind_addr.port();
    let default_port = if scheme == "wss" { 443 } else { 80 };
    if port == default_port {
        format!("{scheme}://{host}/ws")
    } else {
        format!("{scheme}://{host}:{port}/ws")
    }
}

fn pairing_payload(url: &str, secret: &str) -> String {
    serde_json::json!({"url": url, "secret": secret }).to_string()
}

fn print_pairing_info(bind_addr: SocketAddr, secret: &str, advertise_host: Option<IpAddr>) {
    let url = pairing_url(bind_addr, advertise_host);
    println!("  pairing url: {url}");
    // The QR encodes the URL and secret separately so the mobile client can
    // connect with an Authorization header instead of putting the secret in
    // the WebSocket URL (which would be logged by proxies and servers).
    let payload = pairing_payload(&url, secret);
    if let Ok(code) = qrcode::QrCode::new(payload.as_bytes()) {
        let qr = code.render().dark_color('#').light_color(' ').build();
        println!("  pairing QR:");
        for line in qr.lines() {
            println!("    {line}");
        }
    }
}

fn token_hash_eq(token: &str, secret_hash: &[u8; 32]) -> bool {
    let token_hash = blake3::hash(token.as_bytes());
    constant_time_eq::constant_time_eq(secret_hash, token_hash.as_bytes())
}

async fn validate_auth(
    headers: &HeaderMap,
    query: &xai_grok_shell::agent::server::WsQueryParams,
    state: &ProxyState,
) -> bool {
    let tokens: Vec<String> = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .into_iter()
        .chain(query.server_key.iter().map(|s| s.as_str()))
        .map(|s| s.to_string())
        .collect();

    tokens
        .iter()
        .any(|token| token_hash_eq(token, &state.secret_hash))
}

/// Normalize an Origin-like URL so scheme and host are lower-case and the port
/// is omitted when it is the well-known default for the scheme.
fn normalize_origin(raw: &str) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    let scheme = url.scheme().to_ascii_lowercase();
    let host = url.host_str()?.to_ascii_lowercase();
    let port = url.port();
    let default = match scheme.as_str() {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    };
    if port == default || port.is_none() {
        Some(format!("{scheme}://{host}"))
    } else {
        Some(format!("{scheme}://{host}:{}", port?))
    }
}

fn check_origin(
    allowed_origins: &Option<Vec<String>>,
    headers: &HeaderMap,
) -> Result<(), &'static str> {
    let Some(origins) = allowed_origins else {
        return Ok(());
    };
    if origins.is_empty() {
        return Err("allowed origins list is empty");
    }
    if origins.iter().any(|o| o == "*") {
        return Ok(());
    }

    let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        // Non-browser clients (e.g., the omgb CLI) may not send an Origin header.
        return Ok(());
    };

    if origin == "null" {
        if origins.iter().any(|o| o == "null") {
            return Ok(());
        }
        return Err("origin not allowed");
    }

    let Some(origin) = normalize_origin(origin) else {
        return Err("origin not allowed");
    };
    if origins
        .iter()
        .filter_map(|o| normalize_origin(o))
        .any(|o| o == origin)
    {
        return Ok(());
    }
    Err("origin not allowed")
}

async fn prune_rate_limiter(
    rate_limit_per_minute: Option<u32>,
    rate_limiter: &Mutex<HashMap<IpAddr, Vec<Instant>>>,
) {
    if rate_limit_per_minute.is_none() {
        return;
    }
    let now = Instant::now();
    let window = Duration::from_secs(60);
    let mut map = rate_limiter.lock().await;
    let mut empty = Vec::new();
    for (ip, entries) in map.iter_mut() {
        entries.retain(|t| now.saturating_duration_since(*t) < window);
        if entries.is_empty() {
            empty.push(*ip);
        }
    }
    for ip in empty {
        map.remove(&ip);
    }
    if map.len() > MAX_TRACKED_IPS {
        let oldest = map
            .iter()
            .min_by_key(|(_, entries)| entries.last().copied().unwrap_or(now))
            .map(|(ip, _)| *ip);
        if let Some(ip) = oldest {
            map.remove(&ip);
        }
    }
}

async fn cleanup_rate_limiter(
    rate_limit_per_minute: Option<u32>,
    rate_limiter: Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(RATE_LIMIT_CLEANUP_INTERVAL_SECS));
    interval.tick().await;
    loop {
        interval.tick().await;
        prune_rate_limiter(rate_limit_per_minute, &rate_limiter).await;
    }
}

async fn check_rate_limit(
    rate_limit_per_minute: Option<u32>,
    rate_limiter: &Mutex<HashMap<IpAddr, Vec<Instant>>>,
    addr: SocketAddr,
) -> Result<(), &'static str> {
    let Some(limit) = rate_limit_per_minute else {
        return Ok(());
    };
    if limit == 0 {
        return Ok(());
    }

    prune_rate_limiter(Some(limit), rate_limiter).await;

    let now = Instant::now();
    let ip = addr.ip();
    let mut map = rate_limiter.lock().await;
    let entries = map.entry(ip).or_default();
    if entries.len() as u32 >= limit {
        return Err("rate limit exceeded");
    }
    entries.push(now);
    Ok(())
}

async fn find_free_loopback_port() -> Result<SocketAddr> {
    let socket = TcpSocket::new_v4().context("create tcp socket")?;
    socket
        .bind("127.0.0.1:0".parse().context("parse loopback address")?)
        .context("bind loopback socket")?;
    socket.local_addr().context("get local address")
}

async fn spawn_upstream_agent(
    agent_config: xai_grok_shell::agent::config::Config,
    secret: &str,
) -> Result<SocketAddr> {
    for _ in 0..UPSTREAM_PORT_ATTEMPTS {
        let addr = find_free_loopback_port().await?;
        let config = xai_grok_shell::agent::ServerConfig {
            bind_addr: addr,
            secret: secret.to_string(),
        };
        let agent_config = agent_config.clone();
        let handle = tokio::spawn(async move {
            if let Err(e) = xai_grok_shell::agent::run_agent_server(config, agent_config).await {
                warn!("upstream agent server exited: {e}");
            }
        });

        let mut connected = false;
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            if TcpStream::connect(addr).await.is_ok() {
                connected = true;
                break;
            }
        }

        if connected {
            // The handle keeps the upstream server alive; ignore its result.
            std::mem::drop(handle);
            return Ok(addr);
        }
        handle.abort();
    }
    bail!("failed to find free loopback port for upstream agent server")
}

struct ProxyState {
    secret_hash: [u8; 32],
    allowed_origins: Option<Vec<String>>,
    rate_limit_per_minute: Option<u32>,
    rate_limiter: Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>,
    upstream_url: String,
    upstream_secret: String,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<xai_grok_shell::agent::server::WsQueryParams>,
) -> Response {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, msg);
        return (StatusCode::TOO_MANY_REQUESTS, msg).into_response();
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, msg);
        return (StatusCode::FORBIDDEN, msg).into_response();
    }

    if !validate_auth(&headers, &query, &state).await {
        warn!("Unauthorized connection attempt from {}", addr);
        return (
            StatusCode::UNAUTHORIZED,
            "Invalid or missing authorization token",
        )
            .into_response();
    }

    info!("Authenticated WebSocket connection from {}", addr);
    ws.on_upgrade(move |socket| handle_proxy(socket, state))
}

async fn handle_proxy(client_ws: WebSocket, state: Arc<ProxyState>) {
    let (mut client_write, mut client_read) = client_ws.split();
    let upstream_secret = Some(state.upstream_secret.as_str());
    let upstream =
        match crate::net::connect_ws_url(&state.upstream_url, false, upstream_secret).await {
            Ok(s) => s,
            Err(e) => {
                warn!("failed to connect to upstream agent: {e}");
                let _ = client_write
                    .send(Message::Close(Some(CloseFrame {
                        code: 1011,
                        reason: "upstream agent unavailable".into(),
                    })))
                    .await;
                return;
            }
        };
    let (mut up_write, mut up_read) = upstream.split();

    let client_to_up = tokio::spawn(async move {
        while let Some(msg) = client_read.next().await {
            match msg {
                Ok(Message::Text(t)) => {
                    if up_write
                        .send(UpstreamMessage::text(t.as_str()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(Message::Binary(b)) => {
                    if up_write.send(UpstreamMessage::binary(b)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Ping(p)) => {
                    if up_write.send(UpstreamMessage::Ping(p)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Pong(p)) => {
                    if up_write.send(UpstreamMessage::Pong(p)).await.is_err() {
                        break;
                    }
                }
                Ok(Message::Close(frame)) => {
                    let frame = frame.map(|f| UpstreamCloseFrame {
                        code: UpstreamCloseCode::from(f.code),
                        reason: f.reason.as_str().into(),
                    });
                    let _ = up_write.send(UpstreamMessage::Close(frame)).await;
                    break;
                }
                Err(_) => break,
            }
        }
    });

    let up_to_client = tokio::spawn(async move {
        while let Some(msg) = up_read.next().await {
            match msg {
                Ok(UpstreamMessage::Text(t)) => {
                    if client_write.send(Message::text(t.as_str())).await.is_err() {
                        break;
                    }
                }
                Ok(UpstreamMessage::Binary(b)) => {
                    if client_write.send(Message::binary(b)).await.is_err() {
                        break;
                    }
                }
                Ok(UpstreamMessage::Ping(p)) => {
                    if client_write.send(Message::Ping(p)).await.is_err() {
                        break;
                    }
                }
                Ok(UpstreamMessage::Pong(p)) => {
                    if client_write.send(Message::Pong(p)).await.is_err() {
                        break;
                    }
                }
                Ok(UpstreamMessage::Close(frame)) => {
                    let client_msg = frame.map(|f| CloseFrame {
                        code: f.code.into(),
                        reason: f.reason.as_str().into(),
                    });
                    let _ = client_write.send(Message::Close(client_msg)).await;
                    break;
                }
                Err(_) => break,
                _ => continue,
            }
        }
    });

    let _ = tokio::join!(client_to_up, up_to_client);
    info!("WebSocket proxy connection ended");
}

pub async fn serve(args: &ServeArgs) -> Result<()> {
    let mut agent_config = crate::build_agent_config(args.model.clone())?;
    agent_config.default_yolo_mode = args.yolo;

    let (public_secret, secret_path, provided) = match &args.secret {
        Some(s) => {
            if s.len() < 16 {
                bail!("provided secret must be at least 16 characters");
            }
            (s.clone(), None, true)
        }
        None => {
            let dir = omg_dir()?;
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("serve.secret");
            let s = generate_secret();
            std::fs::write(&path, &s)?;
            crate::providers::restrict_env_file_permissions(&path)?;
            (s, Some(path), false)
        }
    };

    let bind_addr = args.bind;
    let advertise_host = args.advertise_host;

    let allowed_origins = if args.allowed_origins.is_empty() {
        None
    } else {
        Some(args.allowed_origins.clone())
    };
    let rate_limit_per_minute = match args.rate_limit {
        None => Some(60),
        Some(0) => None,
        Some(n) => Some(n),
    };

    if !bind_addr.ip().is_loopback() && !args.insecure_allow_lan {
        bail!(
            "serving on a non-loopback address requires --insecure-allow-lan; traffic will not be encrypted"
        );
    }
    if !bind_addr.ip().is_loopback() && allowed_origins.as_ref().is_none_or(|v| v.is_empty()) {
        bail!(
            "serving on a non-loopback address requires --allowed-origins (use '*' to allow any origin)"
        );
    }
    if !bind_addr.ip().is_loopback() {
        eprintln!(
            "warning: omgb serve is listening on a non-loopback address; use a TLS-terminating reverse proxy because the pairing URL uses wss://"
        );
    }

    let upstream_secret = generate_secret();
    let upstream_addr = spawn_upstream_agent(agent_config, &upstream_secret).await?;

    let secret_hash = *blake3::hash(public_secret.as_bytes()).as_bytes();
    let upstream_url = format!("ws://127.0.0.1:{}/ws", upstream_addr.port());
    let state = Arc::new(ProxyState {
        secret_hash,
        allowed_origins,
        rate_limit_per_minute,
        rate_limiter: Arc::new(Mutex::new(HashMap::new())),
        upstream_url,
        upstream_secret,
    });

    tokio::spawn(cleanup_rate_limiter(
        state.rate_limit_per_minute,
        state.rate_limiter.clone(),
    ));

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .with_state(state);
    let listener = TcpListener::bind(bind_addr).await?;
    let actual_addr = listener.local_addr()?;

    println!("oh-my-grok-build serve");
    println!("  bind: {actual_addr}");
    if let Some(ip) = advertise_host {
        println!("  advertise host: {ip}");
    }
    if let Some(path) = &secret_path {
        println!("  secret file: {}", path.display());
    } else if provided {
        println!("  secret: <provided>");
    }
    print_pairing_info(actual_addr, &public_secret, advertise_host);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

pub async fn connect(args: &ConnectArgs) -> Result<()> {
    let mut url = Url::parse(&args.url).map_err(|e| anyhow::anyhow!("invalid URL: {e}"))?;
    match url.scheme() {
        "ws" | "wss" => {}
        "http" => {
            let _ = url.set_scheme("ws");
        }
        "https" => {
            let _ = url.set_scheme("wss");
        }
        _ => anyhow::bail!("URL scheme must be ws, wss, http, or https"),
    }

    if url.path().is_empty() || url.path() == "/" {
        url.set_path("/ws");
    }

    let secret = args.secret.clone().or_else(|| {
        url.query_pairs()
            .find(|(k, _)| k == "server-key")
            .map(|(_, v)| v.into_owned())
    });
    if secret.is_none() {
        anyhow::bail!(
            "--secret is required; use the secret file printed by `omgb serve` or the server-key query parameter"
        );
    }
    url.set_query(None);

    let ws_stream =
        crate::net::connect_ws_url(url.as_str(), args.allow_private, secret.as_deref()).await?;
    println!("Connected to {}", url);

    let (mut write, mut read) = ws_stream.split();
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        tokio::select! {
            res = reader.read_line(&mut line) => {
                match res {
                    Ok(0) => break,
                    Ok(_) => {
                        let text = std::mem::take(&mut line);
                        if text.trim_end().is_empty() {
                            continue;
                        }
                        if write.send(UpstreamMessage::Text(text.trim_end().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => anyhow::bail!("stdin read error: {e}"),
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(UpstreamMessage::Text(t))) => println!("{}", t),
                    Some(Ok(UpstreamMessage::Binary(b))) => println!("{}", String::from_utf8_lossy(&b)),
                    Some(Ok(UpstreamMessage::Close(_))) | None => break,
                    Some(Err(e)) => anyhow::bail!("websocket error: {e}"),
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_secret_length() {
        let s = generate_secret();
        assert_eq!(s.len(), 32);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_format_ip_for_url() {
        assert_eq!(format_ip_for_url("127.0.0.1".parse().unwrap()), "127.0.0.1");
        assert_eq!(format_ip_for_url("::1".parse().unwrap()), "[::1]");
    }

    #[test]
    fn test_pairing_url_no_secret() {
        let bind = SocketAddr::new("0.0.0.0".parse().unwrap(), 2419);
        let host = Some("192.168.1.2".parse().unwrap());
        assert_eq!(pairing_url(bind, host), "wss://192.168.1.2:2419/ws");
    }

    #[test]
    fn test_pairing_url_loopback() {
        let bind = SocketAddr::new("127.0.0.1".parse().unwrap(), 2419);
        assert_eq!(pairing_url(bind, None), "ws://127.0.0.1:2419/ws");
    }

    #[test]
    fn test_pairing_payload() {
        let payload = pairing_payload("wss://192.168.1.2:2419/ws", "abc123");
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["url"], "wss://192.168.1.2:2419/ws");
        assert_eq!(parsed["secret"], "abc123");
    }

    #[test]
    fn test_pairing_host_loopback_v4() {
        let bind = SocketAddr::new("127.0.0.1".parse().unwrap(), 2419);
        assert_eq!(pairing_host(bind, None), "127.0.0.1");
    }

    #[test]
    fn test_pairing_host_loopback_v6() {
        let bind = SocketAddr::new("::1".parse().unwrap(), 2419);
        assert_eq!(pairing_host(bind, None), "[::1]");
    }

    #[test]
    fn test_normalize_origin() {
        assert_eq!(
            normalize_origin("https://Example.com"),
            Some("https://example.com".into())
        );
        assert_eq!(
            normalize_origin("https://example.com:443"),
            Some("https://example.com".into())
        );
        assert_eq!(
            normalize_origin("http://example.com:8080"),
            Some("http://example.com:8080".into())
        );
        assert_eq!(normalize_origin("not-a-url"), None);
    }

    #[test]
    fn test_token_hash_eq() {
        let secret = "super-secret-token";
        let hash = *blake3::hash(secret.as_bytes()).as_bytes();
        assert!(token_hash_eq(secret, &hash));
        assert!(!token_hash_eq("wrong-token", &hash));
    }

    fn test_state(secret: &str) -> Arc<ProxyState> {
        Arc::new(ProxyState {
            secret_hash: *blake3::hash(secret.as_bytes()).as_bytes(),
            allowed_origins: None,
            rate_limit_per_minute: None,
            rate_limiter: Arc::new(Mutex::new(HashMap::new())),
            upstream_url: String::new(),
            upstream_secret: String::new(),
        })
    }

    #[tokio::test]
    async fn test_validate_auth_header() {
        let state = test_state("my-token");
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer my-token".parse().unwrap());
        let query = xai_grok_shell::agent::server::WsQueryParams::default();
        assert!(validate_auth(&headers, &query, &state).await);
    }

    #[tokio::test]
    async fn test_validate_auth_query() {
        let state = test_state("my-token");
        let headers = HeaderMap::new();
        let query = xai_grok_shell::agent::server::WsQueryParams {
            server_key: Some("my-token".into()),
        };
        assert!(validate_auth(&headers, &query, &state).await);
    }
}
