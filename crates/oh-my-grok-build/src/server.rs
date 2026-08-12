//! ACP/WebSocket relay helpers for `omgb serve` and `omgb connect`.
//!
//! `serve` runs a reverse proxy in front of the upstream Grok Build agent
//! server. The proxy adds origin checking, per-IP rate limiting, and
//! constant-time secret verification without modifying the upstream crate.

use std::collections::HashMap;
use std::io::Read;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use axum::{
    Router,
    extract::{
        ConnectInfo, Json, Path, Query, State, ws::CloseFrame, ws::Message, ws::WebSocket,
        ws::WebSocketUpgrade,
    },
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as UpstreamCloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode as UpstreamCloseCode;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};
use url::Url;

use crate::args::{ConnectArgs, ServeArgs};

const RATE_LIMIT_CLEANUP_INTERVAL_SECS: u64 = 60;
const MAX_TRACKED_IPS: usize = 4096;
const MAX_ACTIVE_PROXY_CONNECTIONS: usize = 32;
const MAX_PAIRING_SECRET_BYTES: usize = 512;
const MAX_REMOTE_AGENT_PROMPT_BYTES: usize = 512 * 1024;
const MAX_HOSTED_DISPATCH_CACHE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HOSTED_DISPATCH_CACHE_RECORDS: usize = 1024;
const MAX_HOSTED_DISPATCH_TOMBSTONES: usize = 16 * 1024;
const MAX_HOSTED_DISPATCH_FAILURE_BYTES: usize = 1024;
const MAX_HOSTED_DISPATCH_GATES: usize = 4096;
const MAX_GROUP_MESSAGE_PAGE: usize = 500;
const UPSTREAM_PORT_ATTEMPTS: usize = 20;
const VOICE_START_TIMEOUT: Duration = Duration::from_secs(15);
const VOICE_STT_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const VOICE_SESSION_MAX_DURATION: Duration = Duration::from_secs(5 * 60);
const VOICE_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const VOICE_PCM_FORWARD_TIMEOUT: Duration = Duration::from_secs(2);
const VOICE_CLIENT_SEND_TIMEOUT: Duration = Duration::from_secs(5);
const MIN_VOICE_SAMPLE_RATE: u32 = 8_000;
const MAX_VOICE_SAMPLE_RATE: u32 = 48_000;

fn omg_dir() -> anyhow::Result<std::path::PathBuf> {
    crate::providers::omg_dir()
}

fn generate_secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).context("operating-system random source failed")?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut secret = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        secret.push(HEX[(byte >> 4) as usize] as char);
        secret.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(secret)
}

fn is_valid_pairing_secret(secret: &str) -> bool {
    (16..=MAX_PAIRING_SECRET_BYTES).contains(&secret.len())
        && secret.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

fn read_persisted_secret(path: &std::path::Path) -> Option<String> {
    // Open without following symlinks so an attacker cannot swap the path to a
    // different file between the metadata check and the read.
    let file = open_secret_file(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.is_symlink() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return None;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return None;
        }
    }
    if meta.len() > 1024 {
        return None;
    }
    let mut file = file.take(1024);
    let mut raw = String::new();
    file.read_to_string(&mut raw).ok()?;
    let s = raw.trim();
    if is_valid_pairing_secret(s) {
        Some(s.to_string())
    } else {
        None
    }
}

#[cfg(unix)]
fn open_secret_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_secret_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
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

fn pairing_url(
    bind_addr: SocketAddr,
    advertise_host: Option<IpAddr>,
    advertise_port: Option<u16>,
    wss: bool,
) -> String {
    let host = pairing_host(bind_addr, advertise_host);
    let scheme = if wss { "wss" } else { "ws" };
    let port = advertise_port.unwrap_or(bind_addr.port());
    let default_port = if wss { 443 } else { 80 };
    if port == default_port {
        format!("{scheme}://{host}/ws")
    } else {
        format!("{scheme}://{host}:{port}/ws")
    }
}

fn pairing_payload(url: &str, secret: &str, cwd: Option<&std::path::Path>) -> String {
    let cwd = cwd
        .and_then(std::path::Path::to_str)
        .filter(|path| path.len() <= 1024 && !path.chars().any(char::is_control));
    serde_json::json!({"url": url, "secret": secret, "cwd": cwd }).to_string()
}

fn take_pairing_secret(url: &mut Url, explicit_secret: Option<String>) -> Option<String> {
    let query_secret = url
        .query_pairs()
        .find(|(key, _)| key == "server-key" || key == "server_key")
        .map(|(_, value)| value.into_owned());
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "server-key" && key != "server_key")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    if !retained.is_empty() {
        url.query_pairs_mut().extend_pairs(
            retained
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
    }
    explicit_secret.or(query_secret)
}

fn print_pairing_info(
    bind_addr: SocketAddr,
    secret: &str,
    advertise_host: Option<IpAddr>,
    advertise_port: Option<u16>,
    wss: bool,
) {
    let url = pairing_url(bind_addr, advertise_host, advertise_port, wss);
    println!("  pairing url: {url}");
    // The QR encodes the URL and secret separately so the mobile client can
    // connect with an Authorization header instead of putting the secret in
    // the WebSocket URL (which would be logged by proxies and servers).
    let cwd = std::env::current_dir().ok();
    let payload = pairing_payload(&url, secret, cwd.as_deref());
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
) -> Option<String> {
    let mut tokens: Vec<String> = Vec::new();
    if let Some(token) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        tokens.push(token.to_string());
    }
    if let Some(protos) = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
    {
        for proto in protos.split(',').map(str::trim) {
            if !proto.is_empty() {
                tokens.push(proto.to_string());
            }
        }
    }
    if let Some(ref key) = query.server_key {
        tokens.push(key.clone());
    }

    tokens
        .into_iter()
        .find(|token| token_hash_eq(token, &state.secret_hash))
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

fn cors_layer(allowed_origins: &Option<Vec<String>>) -> Result<Option<CorsLayer>> {
    let Some(origins) = allowed_origins else {
        return Ok(None);
    };
    if origins.is_empty() {
        bail!("allowed origins list is empty");
    }

    let methods = [Method::GET, Method::POST, Method::OPTIONS];
    let headers = [
        header::AUTHORIZATION,
        header::CONTENT_TYPE,
        header::HeaderName::from_static("x-member-token"),
        header::HeaderName::from_static("x-pre-auth-token"),
        header::HeaderName::from_static("x-server-token"),
    ];
    if origins.iter().any(|origin| origin == "*") {
        return Ok(Some(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(methods)
                .allow_headers(headers),
        ));
    }

    let values = origins
        .iter()
        .map(|origin| {
            let normalized = normalize_origin(origin)
                .with_context(|| format!("invalid allowed origin {origin:?}"))?;
            HeaderValue::from_str(&normalized)
                .with_context(|| format!("invalid allowed origin {origin:?}"))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(
        CorsLayer::new()
            .allow_origin(values)
            .allow_methods(methods)
            .allow_headers(headers),
    ))
}

async fn prune_rate_limiter(
    rate_limit_per_minute: Option<u32>,
    rate_limiter: &Mutex<HashMap<IpAddr, Vec<Instant>>>,
) {
    let mut map = rate_limiter.lock().await;
    prune_rate_limiter_locked(rate_limit_per_minute, &mut map, Instant::now());
}

fn prune_rate_limiter_locked(
    rate_limit_per_minute: Option<u32>,
    map: &mut HashMap<IpAddr, Vec<Instant>>,
    now: Instant,
) {
    if rate_limit_per_minute.is_none() {
        return;
    }
    let window = Duration::from_secs(60);
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

    let now = Instant::now();
    let ip = addr.ip();
    let mut map = rate_limiter.lock().await;
    prune_rate_limiter_locked(Some(limit), &mut map, now);
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

struct UpstreamAgent {
    addr: SocketAddr,
    task: JoinHandle<Result<()>>,
}

impl Drop for UpstreamAgent {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_upstream_agent(
    agent_config: xai_grok_shell::agent::config::Config,
    secret: &str,
) -> Result<UpstreamAgent> {
    for _ in 0..UPSTREAM_PORT_ATTEMPTS {
        let addr = find_free_loopback_port().await?;
        let config = xai_grok_shell::agent::ServerConfig {
            bind_addr: addr,
            secret: secret.to_string(),
        };
        let agent_config = agent_config.clone();
        let handle = tokio::spawn(async move {
            xai_grok_shell::agent::run_agent_server(config, agent_config)
                .await
                .context("upstream agent server failed")
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
            return Ok(UpstreamAgent { addr, task: handle });
        }
        handle.abort();
    }
    bail!("failed to find free loopback port for upstream agent server")
}

fn upstream_exit_result(
    result: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> Result<()> {
    match result {
        Ok(Ok(())) => bail!("upstream agent server exited unexpectedly"),
        Ok(Err(error)) => Err(error),
        Err(error) if error.is_cancelled() => bail!("upstream agent server was cancelled"),
        Err(error) => Err(error).context("upstream agent server task failed"),
    }
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    version: &'static str,
}

const RELAY_API_VERSION: u32 = 1;
const RELAY_CAPABILITIES: &[&str] = &[
    "acp.session.resume",
    "acp.session.page",
    "group.dispatch.status.v1",
    "group.history.cursor.v1",
    "group.join.ack.v1",
    "relay.status.v1",
    "voice.pcm.v1",
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayLimits {
    group_message_bytes: usize,
    group_message_page: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayCapabilitiesResponse {
    service: &'static str,
    version: &'static str,
    api_version: u32,
    capabilities: &'static [&'static str],
    limits: RelayLimits,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RelayStatusResponse {
    status: &'static str,
    service: &'static str,
    version: &'static str,
    api_version: u32,
    uptime_seconds: u64,
    active_connections: usize,
    connection_limit: usize,
    local_hosted_dispatches: usize,
    rate_limit_per_minute: Option<u32>,
}

async fn health_handler() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "oh-my-grok-build-relay",
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn capabilities_handler() -> Json<RelayCapabilitiesResponse> {
    Json(RelayCapabilitiesResponse {
        service: "oh-my-grok-build-relay",
        version: env!("CARGO_PKG_VERSION"),
        api_version: RELAY_API_VERSION,
        capabilities: RELAY_CAPABILITIES,
        limits: RelayLimits {
            group_message_bytes: crate::group::MAX_GROUP_MESSAGE_BYTES,
            group_message_page: MAX_GROUP_MESSAGE_PAGE,
        },
    })
}

async fn relay_status(state: &ProxyState) -> RelayStatusResponse {
    let active_connections =
        MAX_ACTIVE_PROXY_CONNECTIONS.saturating_sub(state.connection_limit.available_permits());
    let local_hosted_dispatches = state
        .hosted_dispatch_gates
        .lock()
        .await
        .values()
        .filter(|gate| Arc::strong_count(gate) > 1)
        .count();
    RelayStatusResponse {
        status: "ready",
        service: "oh-my-grok-build-relay",
        version: env!("CARGO_PKG_VERSION"),
        api_version: RELAY_API_VERSION,
        uptime_seconds: state.started_at.elapsed().as_secs(),
        active_connections,
        connection_limit: MAX_ACTIVE_PROXY_CONNECTIONS,
        local_hosted_dispatches,
        rate_limit_per_minute: state.rate_limit_per_minute,
    }
}

fn relay_voice_config(
    agent_config: &xai_grok_shell::agent::config::Config,
) -> xai_grok_voice::VoiceConfig {
    let raw = xai_grok_shell::config::load_effective_config_disk_only().ok();
    let mut config = raw
        .as_ref()
        .and_then(toml::Value::as_table)
        .map(|root| {
            xai_grok_voice::VoiceConfig::from_config_table(
                root,
                Some(&agent_config.endpoints.xai_api_base_url),
            )
        })
        .unwrap_or_default();
    config.client_identifier = "oh-my-grok-build-mobile".to_string();
    config.user_agent = format!("oh-my-grok-build/{}", env!("CARGO_PKG_VERSION"));
    config
}

struct ProxyState {
    secret_hash: [u8; 32],
    allowed_origins: Option<Vec<String>>,
    rate_limit_per_minute: Option<u32>,
    rate_limiter: Arc<Mutex<HashMap<IpAddr, Vec<Instant>>>>,
    connection_limit: Arc<Semaphore>,
    upstream_url: String,
    upstream_secret: String,
    voice_config: xai_grok_voice::VoiceConfig,
    voice_auth: xai_grok_voice::SharedVoiceAuth,
    hosted_dispatch_gates: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    started_at: Instant,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum VoiceClientMessage {
    Start {
        sample_rate: u32,
        channels: u8,
        encoding: String,
    },
    Stop,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum VoiceServerMessage<'a> {
    Ready,
    Partial {
        text: &'a str,
        is_final: bool,
        speech_final: bool,
    },
    Done {
        text: &'a str,
    },
    Error {
        message: &'a str,
    },
}

fn validate_voice_start(message: VoiceClientMessage) -> Result<u32, &'static str> {
    let VoiceClientMessage::Start {
        sample_rate,
        channels,
        encoding,
    } = message
    else {
        return Err("expected voice start message");
    };
    if !(MIN_VOICE_SAMPLE_RATE..=MAX_VOICE_SAMPLE_RATE).contains(&sample_rate) {
        return Err("voice sample rate is unsupported");
    }
    if channels != 1 {
        return Err("voice input must be mono");
    }
    if encoding != "int16" {
        return Err("voice input must be signed 16-bit PCM");
    }
    Ok(sample_rate)
}

async fn send_voice_event(
    writer: &mut futures::stream::SplitSink<WebSocket, Message>,
    event: VoiceServerMessage<'_>,
) -> bool {
    match serde_json::to_string(&event) {
        Ok(body) => tokio::time::timeout(
            VOICE_CLIENT_SEND_TIMEOUT,
            writer.send(Message::Text(body.into())),
        )
        .await
        .is_ok_and(|result| result.is_ok()),
        Err(_) => false,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum VoicePcmForwardError {
    SessionDeadline,
    Backpressure,
    Closed,
}

async fn bounded_voice_pcm_forward<F, E>(
    session_deadline: tokio::time::Instant,
    forward_timeout: Duration,
    send: F,
) -> std::result::Result<(), VoicePcmForwardError>
where
    F: std::future::Future<Output = std::result::Result<(), E>>,
{
    let remaining = session_deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Err(VoicePcmForwardError::SessionDeadline);
    }
    match tokio::time::timeout(remaining.min(forward_timeout), send).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(VoicePcmForwardError::Closed),
        Err(_) if tokio::time::Instant::now() >= session_deadline => {
            Err(VoicePcmForwardError::SessionDeadline)
        }
        Err(_) => Err(VoicePcmForwardError::Backpressure),
    }
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

    let Some(matched_token) = validate_auth(&headers, &query, &state).await else {
        warn!("Unauthorized connection attempt from {}", addr);
        return (
            StatusCode::UNAUTHORIZED,
            "Invalid or missing authorization token",
        )
            .into_response();
    };
    let permit = match state.connection_limit.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            warn!("Connection limit reached for {}", addr);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "relay connection capacity reached; retry shortly",
            )
                .into_response();
        }
    };

    let from_protocol = headers.get("sec-websocket-protocol").is_some_and(|v| {
        v.to_str().ok().is_some_and(|protos| {
            protos
                .split(',')
                .map(str::trim)
                .any(|p| p == matched_token.as_str())
        })
    });

    info!("Authenticated WebSocket connection from {}", addr);
    let ws = if from_protocol {
        ws.protocols([matched_token])
    } else {
        ws
    };
    ws.max_message_size(crate::net::MAX_WEBSOCKET_MESSAGE_BYTES)
        .max_frame_size(crate::net::MAX_WEBSOCKET_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            handle_proxy(socket, state).await;
        })
}

async fn voice_ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<xai_grok_shell::agent::server::WsQueryParams>,
) -> Response {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Voice rate limit exceeded for {}: {}", addr, msg);
        return (StatusCode::TOO_MANY_REQUESTS, msg).into_response();
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Voice origin check failed for {}: {}", addr, msg);
        return (StatusCode::FORBIDDEN, msg).into_response();
    }

    let Some(matched_token) = validate_auth(&headers, &query, &state).await else {
        warn!("Unauthorized voice connection attempt from {}", addr);
        return (
            StatusCode::UNAUTHORIZED,
            "Invalid or missing authorization token",
        )
            .into_response();
    };
    let permit = match state.connection_limit.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            warn!("Voice connection limit reached for {}", addr);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "relay connection capacity reached; retry shortly",
            )
                .into_response();
        }
    };

    let from_protocol = headers.get("sec-websocket-protocol").is_some_and(|v| {
        v.to_str().ok().is_some_and(|protos| {
            protos
                .split(',')
                .map(str::trim)
                .any(|p| p == matched_token.as_str())
        })
    });
    let ws = if from_protocol {
        ws.protocols([matched_token])
    } else {
        ws
    };
    ws.max_message_size(crate::net::MAX_WEBSOCKET_MESSAGE_BYTES)
        .max_frame_size(crate::net::MAX_WEBSOCKET_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            handle_voice(socket, state).await;
        })
}

async fn handle_voice(client_ws: WebSocket, state: Arc<ProxyState>) {
    use xai_grok_voice::stt::{StreamingSttEvent, StreamingSttSession};

    let (mut write, mut read) = client_ws.split();
    let start = match tokio::time::timeout(VOICE_START_TIMEOUT, read.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str::<VoiceClientMessage>(&text)
            .map_err(|_| "invalid voice start message")
            .and_then(validate_voice_start),
        Ok(Some(Ok(_))) => Err("expected voice start message"),
        Ok(Some(Err(_))) | Ok(None) => return,
        Err(_) => Err("voice start timed out"),
    };
    let sample_rate = match start {
        Ok(sample_rate) => sample_rate,
        Err(message) => {
            let _ = send_voice_event(&mut write, VoiceServerMessage::Error { message }).await;
            return;
        }
    };

    let Some(bearer) = state.voice_auth.bearer().await else {
        let _ = send_voice_event(
            &mut write,
            VoiceServerMessage::Error {
                message: "voice requires a signed-in xAI account or API key on the harness",
            },
        )
        .await;
        return;
    };
    let mut config = state.voice_config.clone();
    config.sample_rate = sample_rate;
    let mut stt = match tokio::time::timeout(
        VOICE_STT_CONNECT_TIMEOUT,
        StreamingSttSession::connect(&config, &bearer),
    )
    .await
    {
        Ok(Ok(session)) => session,
        Ok(Err(error)) => {
            warn!("Voice STT connection failed: {error}");
            let _ = send_voice_event(
                &mut write,
                VoiceServerMessage::Error {
                    message: "voice transcription service is unavailable",
                },
            )
            .await;
            return;
        }
        Err(_) => {
            warn!("Voice STT connection timed out");
            let _ = send_voice_event(
                &mut write,
                VoiceServerMessage::Error {
                    message: "voice transcription service connection timed out",
                },
            )
            .await;
            return;
        }
    };
    if !send_voice_event(&mut write, VoiceServerMessage::Ready).await {
        return;
    }

    let session_deadline = tokio::time::Instant::now() + VOICE_SESSION_MAX_DURATION;
    let never = tokio::time::Instant::now() + Duration::from_secs(24 * 60 * 60);
    let mut accepting_audio = true;
    let mut drain_deadline = None;

    loop {
        let finish_at = drain_deadline.unwrap_or(never);
        tokio::select! {
            message = read.next(), if accepting_audio => {
                match message {
                    Some(Ok(Message::Binary(pcm))) => {
                        if pcm.is_empty() || pcm.len() % 2 != 0 {
                            let _ = send_voice_event(&mut write, VoiceServerMessage::Error {
                                message: "voice PCM frames must be non-empty signed 16-bit samples",
                            }).await;
                            break;
                        }
                        match bounded_voice_pcm_forward(
                            session_deadline,
                            VOICE_PCM_FORWARD_TIMEOUT,
                            stt.send_pcm(pcm.to_vec()),
                        ).await {
                            Ok(()) => {}
                            Err(VoicePcmForwardError::SessionDeadline) => {
                                stt.finish_audio();
                                accepting_audio = false;
                                drain_deadline = Some(tokio::time::Instant::now() + VOICE_DRAIN_TIMEOUT);
                                if !send_voice_event(&mut write, VoiceServerMessage::Error {
                                    message: "voice session reached its five-minute limit",
                                }).await {
                                    break;
                                }
                            }
                            Err(VoicePcmForwardError::Backpressure) => {
                                stt.finish_audio();
                                let _ = send_voice_event(&mut write, VoiceServerMessage::Error {
                                    message: "voice transcription service stopped accepting audio",
                                }).await;
                                break;
                            }
                            Err(VoicePcmForwardError::Closed) => {
                                let _ = send_voice_event(&mut write, VoiceServerMessage::Error {
                                    message: "voice transcription connection closed",
                                }).await;
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<VoiceClientMessage>(&text) {
                        Ok(VoiceClientMessage::Stop) => {
                            stt.finish_audio();
                            accepting_audio = false;
                            drain_deadline = Some(tokio::time::Instant::now() + VOICE_DRAIN_TIMEOUT);
                        }
                        _ => {
                            let _ = send_voice_event(&mut write, VoiceServerMessage::Error {
                                message: "unexpected voice control message",
                            }).await;
                            break;
                        }
                    },
                    Some(Ok(Message::Ping(payload))) => {
                        if !tokio::time::timeout(
                            VOICE_CLIENT_SEND_TIMEOUT,
                            write.send(Message::Pong(payload)),
                        )
                        .await
                        .is_ok_and(|result| result.is_ok())
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        stt.finish_audio();
                        accepting_audio = false;
                        drain_deadline = Some(tokio::time::Instant::now() + VOICE_DRAIN_TIMEOUT);
                    }
                }
            }
            event = stt.recv() => match event {
                Some(StreamingSttEvent::Partial(partial)) => {
                    if !send_voice_event(&mut write, VoiceServerMessage::Partial {
                        text: &partial.text,
                        is_final: partial.is_final,
                        speech_final: partial.speech_final,
                    }).await {
                        break;
                    }
                }
                Some(StreamingSttEvent::Done { text }) => {
                    let _ = send_voice_event(&mut write, VoiceServerMessage::Done { text: &text }).await;
                    break;
                }
                Some(StreamingSttEvent::Error { message }) => {
                    warn!("Voice STT session failed: {message}");
                    let _ = send_voice_event(&mut write, VoiceServerMessage::Error {
                        message: "voice transcription failed",
                    }).await;
                    break;
                }
                Some(StreamingSttEvent::Ready) | None => break,
            },
            _ = tokio::time::sleep_until(session_deadline), if accepting_audio => {
                stt.finish_audio();
                accepting_audio = false;
                drain_deadline = Some(tokio::time::Instant::now() + VOICE_DRAIN_TIMEOUT);
                if !send_voice_event(&mut write, VoiceServerMessage::Error {
                    message: "voice session reached its five-minute limit",
                }).await {
                    break;
                }
            }
            _ = tokio::time::sleep_until(finish_at), if drain_deadline.is_some() => break,
        }
    }
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

    finish_proxy_bridge(client_to_up, up_to_client).await;
    info!("WebSocket proxy connection ended");
}

async fn finish_proxy_bridge(
    mut client_to_up: tokio::task::JoinHandle<()>,
    mut up_to_client: tokio::task::JoinHandle<()>,
) {
    tokio::select! {
        _ = &mut client_to_up => {
            up_to_client.abort();
            let _ = up_to_client.await;
        }
        _ = &mut up_to_client => {
            client_to_up.abort();
            let _ = client_to_up.await;
        }
    }
}

#[derive(Deserialize)]
struct GroupTokenQuery {
    token: Option<String>,
}

#[derive(Deserialize)]
struct GroupMessagesQuery {
    after: Option<String>,
    before: Option<String>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct GroupInfo {
    id: String,
    name: String,
    description: String,
    model: String,
    yolo: bool,
    host_name: String,
    agents: Vec<crate::group::Agent>,
    members: Vec<String>,
    pending_joins: Vec<PublicJoinRequest>,
    remote_agents: Vec<PublicRemoteAgent>,
}

#[derive(Serialize)]
struct PublicRemoteAgent {
    name: String,
    role: String,
    model: String,
    last_heartbeat: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct PublicJoinRequest {
    id: String,
    name: String,
    github: Option<String>,
    requested_at: DateTime<Utc>,
}

impl From<&crate::group::JoinRequest> for PublicJoinRequest {
    fn from(r: &crate::group::JoinRequest) -> Self {
        Self {
            id: r.id.clone(),
            name: r.name.clone(),
            github: r.github.clone(),
            requested_at: r.requested_at,
        }
    }
}

#[derive(Deserialize)]
struct GroupMessagePayload {
    content: String,
    kind: crate::group::MessageKind,
    #[serde(default)]
    client_message_id: Option<String>,
}

#[derive(Deserialize)]
struct CreateGroupPayload {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    count: Option<usize>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    human_name: Option<String>,
    #[serde(default)]
    yolo: bool,
}

#[derive(Serialize)]
struct CreateGroupResponse {
    id: String,
    token: String,
    host_member_token: String,
    name: String,
    description: String,
    model: String,
    yolo: bool,
    host_name: String,
    agents: Vec<crate::group::Agent>,
    members: Vec<String>,
    pending_joins: Vec<PublicJoinRequest>,
}

#[derive(Deserialize)]
struct CreateWorkflowPayload {
    prompt: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    yolo: bool,
}

#[derive(Serialize)]
struct CreateWorkflowResponse {
    name: String,
    path: String,
}

fn extract_server_token(headers: &HeaderMap) -> String {
    if let Some(t) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| !v.is_empty())
    {
        return t.to_string();
    }
    if let Some(t) = headers
        .get("x-server-token")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
    {
        return t.to_string();
    }
    String::new()
}

fn extract_group_token_from_headers(headers: &HeaderMap) -> String {
    for header in ["x-member-token", "x-group-token"] {
        if let Some(t) = headers
            .get(header)
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty())
        {
            return t.to_string();
        }
    }
    if let Some(t) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| !v.is_empty())
    {
        return t.to_string();
    }
    String::new()
}

fn extract_group_token(query: &GroupTokenQuery, headers: &HeaderMap) -> String {
    let header_token = extract_group_token_from_headers(headers);
    if !header_token.is_empty() {
        return header_token;
    }
    if let Some(t) = query.token.as_deref()
        && !t.is_empty()
    {
        return t.to_string();
    }
    String::new()
}

fn is_not_found(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
}

fn group_token_valid(group: &crate::group::Group, token: &str) -> bool {
    if constant_time_eq::constant_time_eq(group.invite_token.as_bytes(), token.as_bytes()) {
        return true;
    }
    crate::group::validate_member_token(group, token).is_some()
}

fn is_member_token(group: &crate::group::Group, token: &str) -> bool {
    crate::group::validate_member_token(group, token).is_some()
}

fn is_host_member_token(group: &crate::group::Group, token: &str) -> bool {
    crate::group::is_host_member_token(group, token)
}

fn server_token_valid(state: &ProxyState, token: &str) -> bool {
    constant_time_eq::constant_time_eq(
        state.secret_hash.as_ref(),
        blake3::hash(token.as_bytes()).as_bytes(),
    )
}

async fn relay_status_handler(
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<RelayStatusResponse>, (StatusCode, String)> {
    if let Err(message) =
        check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, message);
        return Err((StatusCode::TOO_MANY_REQUESTS, message.to_string()));
    }
    if let Err(message) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, message);
        return Err((StatusCode::FORBIDDEN, message.to_string()));
    }
    if !server_token_valid(&state, &extract_server_token(&headers)) {
        return Err((StatusCode::UNAUTHORIZED, "invalid server token".to_string()));
    }
    Ok(Json(relay_status(&state).await))
}

async fn admin_create_group_handler(
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<CreateGroupPayload>,
) -> Result<Json<CreateGroupResponse>, (StatusCode, String)> {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, msg);
        return Err((StatusCode::TOO_MANY_REQUESTS, msg.to_string()));
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, msg);
        return Err((StatusCode::FORBIDDEN, msg.to_string()));
    }
    let token = extract_server_token(&headers);
    if !server_token_valid(&state, &token) {
        return Err((StatusCode::UNAUTHORIZED, "invalid server token".to_string()));
    }

    let args = crate::args::GroupNewArgs {
        name: payload.name,
        description: Some(payload.description).filter(|d| !d.is_empty()),
        count: payload.count,
        model: payload.model,
        names: payload.names,
        roles: payload.roles,
        models: payload.models,
        human_name: payload.human_name.filter(|h| !h.is_empty()),
        yolo: payload.yolo,
    };

    let spec = crate::group::validate_group_create(&args)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let group = tokio::task::spawn_blocking(move || crate::group::build_group(&args, spec))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let host_member_token = group
        .member_tokens
        .get(&group.host_name)
        .cloned()
        .unwrap_or_else(crate::group::generate_member_token);

    Ok(Json(CreateGroupResponse {
        id: group.id,
        token: group.invite_token,
        host_member_token,
        name: group.name,
        description: group.description,
        model: group.model,
        yolo: group.yolo,
        host_name: group.host_name,
        agents: group.agents,
        members: group.members,
        pending_joins: group.pending_joins.iter().map(|r| r.into()).collect(),
    }))
}

async fn admin_create_workflow_handler(
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<CreateWorkflowPayload>,
) -> Result<Json<CreateWorkflowResponse>, (StatusCode, String)> {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, msg);
        return Err((StatusCode::TOO_MANY_REQUESTS, msg.to_string()));
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, msg);
        return Err((StatusCode::FORBIDDEN, msg.to_string()));
    }
    let token = extract_server_token(&headers);
    if !server_token_valid(&state, &token) {
        return Err((StatusCode::UNAUTHORIZED, "invalid server token".to_string()));
    }

    let args = crate::args::WorkflowCreateArgs {
        prompt: payload.prompt,
        name: payload.name,
        model: payload.model,
        yolo: payload.yolo,
        dry_run: false,
    };

    let (name, path) = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(crate::workflow::create_workflow(&args))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(Json(CreateWorkflowResponse {
        name,
        path: path.to_string_lossy().into_owned(),
    }))
}

async fn group_info_handler(
    Path(id): Path<String>,
    Query(query): Query<GroupTokenQuery>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<GroupInfo>, (StatusCode, &'static str)> {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, msg);
        return Err((StatusCode::TOO_MANY_REQUESTS, msg));
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, msg);
        return Err((StatusCode::FORBIDDEN, msg));
    }
    if crate::threads::validate_id(&id).is_err() {
        return Err((StatusCode::BAD_REQUEST, "invalid group id"));
    }
    let token = extract_group_token(&query, &headers);
    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found"))?;
    if !group_token_valid(&group, &token) {
        return Err((StatusCode::UNAUTHORIZED, "invalid token"));
    }
    let is_member = is_member_token(&group, &token);
    let is_host = is_host_member_token(&group, &token);
    Ok(Json(GroupInfo {
        id: group.id,
        name: group.name,
        description: group.description,
        model: group.model,
        yolo: group.yolo,
        host_name: group.host_name,
        agents: if is_member { group.agents } else { Vec::new() },
        members: if is_member { group.members } else { Vec::new() },
        pending_joins: if is_host {
            group.pending_joins.iter().map(|r| r.into()).collect()
        } else {
            Vec::new()
        },
        remote_agents: if is_member {
            group
                .remote_agents
                .iter()
                .map(|r| PublicRemoteAgent {
                    name: r.name.clone(),
                    role: r.role.clone(),
                    model: r.model.clone(),
                    last_heartbeat: r.last_heartbeat,
                })
                .collect()
        } else {
            Vec::new()
        },
    }))
}

async fn group_list_messages_handler(
    Path(id): Path<String>,
    Query(query): Query<GroupMessagesQuery>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<Vec<crate::group::GroupMessage>>, (StatusCode, &'static str)> {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, msg);
        return Err((StatusCode::TOO_MANY_REQUESTS, msg));
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, msg);
        return Err((StatusCode::FORBIDDEN, msg));
    }
    if crate::threads::validate_id(&id).is_err() {
        return Err((StatusCode::BAD_REQUEST, "invalid group id"));
    }
    let limit = query.limit.unwrap_or(200);
    if !(1..=MAX_GROUP_MESSAGE_PAGE).contains(&limit) {
        return Err((StatusCode::BAD_REQUEST, "invalid message page size"));
    }
    if query
        .after
        .as_deref()
        .is_some_and(|after| crate::threads::validate_id(after).is_err())
    {
        return Err((StatusCode::BAD_REQUEST, "invalid last message id"));
    }
    if query
        .before
        .as_deref()
        .is_some_and(|before| crate::threads::validate_id(before).is_err())
    {
        return Err((StatusCode::BAD_REQUEST, "invalid first message id"));
    }
    if query.after.is_some() && query.before.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "after and before are mutually exclusive",
        ));
    }
    let token = extract_group_token_from_headers(&headers);
    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found"))?;
    if !is_member_token(&group, &token) {
        return Err((StatusCode::UNAUTHORIZED, "a valid member token is required"));
    }
    let messages = crate::group::load_message_page_async(
        &id,
        query.after.as_deref(),
        query.before.as_deref(),
        limit,
    )
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to load messages"))?;
    let messages = messages.ok_or((StatusCode::CONFLICT, "message cursor expired"))?;
    Ok(Json(messages))
}

async fn group_post_message_handler(
    Path(id): Path<String>,
    Query(_query): Query<GroupTokenQuery>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<GroupMessagePayload>,
) -> Result<StatusCode, (StatusCode, String)> {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, msg);
        return Err((StatusCode::TOO_MANY_REQUESTS, msg.to_string()));
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, msg);
        return Err((StatusCode::FORBIDDEN, msg.to_string()));
    }
    if let Err(e) = crate::threads::validate_id(&id) {
        return Err((StatusCode::BAD_REQUEST, e.to_string()));
    }
    let token = extract_group_token_from_headers(&headers);
    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found".to_string()))?;
    let sender = crate::group::validate_member_token(&group, &token).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            "a valid member token is required to post".to_string(),
        )
    })?;
    if !matches!(
        payload.kind,
        crate::group::MessageKind::User | crate::group::MessageKind::Human
    ) {
        return Err((
            StatusCode::BAD_REQUEST,
            "message kind must be user or human".to_string(),
        ));
    }
    if group
        .agents
        .iter()
        .any(|a| a.name.eq_ignore_ascii_case(&sender))
        || group
            .remote_agents
            .iter()
            .any(|r| r.name.eq_ignore_ascii_case(&sender))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "sender name conflicts with an agent".to_string(),
        ));
    }
    let content = payload.content.trim().to_string();
    crate::group::validate_message_content(&content).map_err(|e| {
        let status = if content.len() > crate::group::MAX_GROUP_MESSAGE_BYTES {
            StatusCode::PAYLOAD_TOO_LARGE
        } else {
            StatusCode::BAD_REQUEST
        };
        (status, e.to_string())
    })?;
    let client_message_id = payload
        .client_message_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "client_message_id is required for idempotent delivery".to_string(),
            )
        })?;
    if crate::threads::validate_id(&client_message_id).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid client message id".to_string(),
        ));
    }
    let message_id = client_message_id.clone();
    let message = crate::group::GroupMessage {
        id: message_id,
        timestamp: Utc::now(),
        sender,
        content,
        kind: payload.kind,
        client_message_id: Some(client_message_id),
        reply_to: None,
    };
    crate::group::persist_and_queue_message_async(&id, &message, &message.sender)
        .await
        .map_err(|error| {
            if error.to_string().contains("different payload") {
                (
                    StatusCode::CONFLICT,
                    "message id is already associated with different content".to_string(),
                )
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "failed to save and queue message".to_string(),
                )
            }
        })?;

    // The durable record is committed before acknowledgement. The worker may
    // finish after the response and is recovered from the ledger on restart.
    if let Err(error) = crate::group::schedule_group_dispatch(id.clone()) {
        eprintln!("warning: message is durable but dispatch scheduling failed: {error}");
    }

    Ok(StatusCode::CREATED)
}

async fn group_dispatch_status_handler(
    Path((id, trigger_id)): Path<(String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<crate::group::GroupDispatchStatus>, (StatusCode, String)> {
    if let Err(message) =
        check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        return Err((StatusCode::TOO_MANY_REQUESTS, message.to_string()));
    }
    check_origin(&state.allowed_origins, &headers)
        .map_err(|error| (StatusCode::FORBIDDEN, error.to_string()))?;
    crate::threads::validate_id(&id)
        .and_then(|_| crate::threads::validate_id(&trigger_id))
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let token = extract_group_token_from_headers(&headers);
    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found".to_string()))?;
    if !is_member_token(&group, &token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "a valid member token is required".to_string(),
        ));
    }
    crate::group::dispatch_status_async(&id, &trigger_id)
        .await
        .map_err(|error| {
            warn!("group dispatch status failed: {error}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to load dispatch status".to_string(),
            )
        })?
        .map(Json)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "dispatch status not found or expired".to_string(),
            )
        })
}

#[derive(Deserialize)]
struct GroupDispatchStatusesQuery {
    ids: String,
}

async fn group_dispatch_statuses_handler(
    Path(id): Path<String>,
    Query(query): Query<GroupDispatchStatusesQuery>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<Vec<crate::group::GroupDispatchStatus>>, (StatusCode, String)> {
    if let Err(message) =
        check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        return Err((StatusCode::TOO_MANY_REQUESTS, message.to_string()));
    }
    check_origin(&state.allowed_origins, &headers)
        .map_err(|error| (StatusCode::FORBIDDEN, error.to_string()))?;
    crate::threads::validate_id(&id)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let ids = query
        .ids
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if ids.is_empty()
        || ids.len() > 20
        || ids
            .iter()
            .any(|trigger_id| crate::threads::validate_id(trigger_id).is_err())
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "ids must contain 1 to 20 valid dispatch ids".to_string(),
        ));
    }
    let token = extract_group_token_from_headers(&headers);
    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found".to_string()))?;
    if !is_member_token(&group, &token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "a valid member token is required".to_string(),
        ));
    }
    crate::group::dispatch_statuses_async(&id, ids)
        .await
        .map(Json)
        .map_err(|error| {
            warn!("group dispatch status batch failed: {error}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to load dispatch statuses".to_string(),
            )
        })
}

async fn group_dispatch_retry_handler(
    Path((id, trigger_id)): Path<(String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<StatusCode, (StatusCode, String)> {
    if let Err(message) =
        check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        return Err((StatusCode::TOO_MANY_REQUESTS, message.to_string()));
    }
    check_origin(&state.allowed_origins, &headers)
        .map_err(|error| (StatusCode::FORBIDDEN, error.to_string()))?;
    crate::threads::validate_id(&id)
        .and_then(|_| crate::threads::validate_id(&trigger_id))
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let token = extract_group_token_from_headers(&headers);
    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found".to_string()))?;
    let requester = crate::group::validate_member_token(&group, &token).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            "a valid member token is required".to_string(),
        )
    })?;
    let current = crate::group::dispatch_status_async(&id, &trigger_id)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to load dispatch status".to_string(),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "dispatch status not found or expired".to_string(),
            )
        })?;
    if !requester.eq_ignore_ascii_case(&current.human_name)
        && !crate::group::is_host_member_token(&group, &token)
    {
        return Err((
            StatusCode::FORBIDDEN,
            "only the original sender or group host can retry this dispatch".to_string(),
        ));
    }
    let requeued = crate::group::retry_dispatch_async(&id, &trigger_id)
        .await
        .map_err(|error| {
            let message = error.to_string();
            if message.contains("ambiguous") {
                (StatusCode::CONFLICT, message)
            } else {
                warn!("group dispatch retry failed: {error}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "failed to retry dispatch".to_string(),
                )
            }
        })?;
    if !requeued {
        return Err((
            StatusCode::CONFLICT,
            "only a failed retryable dispatch can be retried".to_string(),
        ));
    }
    if let Err(error) = crate::group::schedule_group_dispatch(id.clone()) {
        eprintln!("warning: dispatch retry is durable but scheduling failed: {error}");
    }
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct JoinPayload {
    name: String,
    #[serde(default)]
    github: Option<String>,
}

async fn group_list_joins_handler(
    Path(id): Path<String>,
    Query(_query): Query<GroupTokenQuery>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<Vec<PublicJoinRequest>>, (StatusCode, String)> {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, msg);
        return Err((StatusCode::TOO_MANY_REQUESTS, msg.to_string()));
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, msg);
        return Err((StatusCode::FORBIDDEN, msg.to_string()));
    }
    if let Err(e) = crate::threads::validate_id(&id) {
        return Err((StatusCode::BAD_REQUEST, e.to_string()));
    }
    let token = extract_group_token_from_headers(&headers);
    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found".to_string()))?;
    if !is_host_member_token(&group, &token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "the group host token is required".to_string(),
        ));
    }
    Ok(Json(group.pending_joins.iter().map(|r| r.into()).collect()))
}

async fn group_request_join_handler(
    Path(id): Path<String>,
    Query(query): Query<GroupTokenQuery>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<JoinPayload>,
) -> Result<Json<crate::group::JoinResult>, (StatusCode, String)> {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, msg);
        return Err((StatusCode::TOO_MANY_REQUESTS, msg.to_string()));
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, msg);
        return Err((StatusCode::FORBIDDEN, msg.to_string()));
    }
    if let Err(e) = crate::threads::validate_id(&id) {
        return Err((StatusCode::BAD_REQUEST, e.to_string()));
    }
    let token = extract_group_token(&query, &headers);
    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found".to_string()))?;
    if !group_token_valid(&group, &token) {
        return Err((StatusCode::UNAUTHORIZED, "invalid token".to_string()));
    }
    let name = payload.name.trim().to_string();
    let github = payload
        .github
        .as_deref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Err(e) = crate::group::validate_human_name(&name) {
        return Err((StatusCode::BAD_REQUEST, e.to_string()));
    }

    let result = crate::group::modify_group_async(&id, move |group| {
        crate::group::request_join(group, &name, github.as_deref())
    })
    .await
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(Json(result))
}

#[derive(Debug)]
enum ApproveJoinError {
    InvalidToken,
    RequestNotFound,
    BadRequest(String),
}

impl std::fmt::Display for ApproveJoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidToken => f.write_str("the group host token is required to approve"),
            Self::RequestNotFound => f.write_str("join request not found"),
            Self::BadRequest(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ApproveJoinError {}

async fn group_approve_join_handler(
    Path((id, request_id)): Path<(String, String)>,
    Query(_query): Query<GroupTokenQuery>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<crate::group::JoinResult>, (StatusCode, String)> {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, msg);
        return Err((StatusCode::TOO_MANY_REQUESTS, msg.to_string()));
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, msg);
        return Err((StatusCode::FORBIDDEN, msg.to_string()));
    }
    if let Err(e) = crate::threads::validate_id(&id) {
        return Err((StatusCode::BAD_REQUEST, e.to_string()));
    }
    if let Err(e) = crate::threads::validate_id(&request_id) {
        return Err((StatusCode::BAD_REQUEST, e.to_string()));
    }

    let token = extract_group_token_from_headers(&headers);

    let modify_request_id = request_id.clone();
    let (name, _) = crate::group::modify_group_async(&id, move |group| {
        if !crate::group::is_host_member_token(group, &token) {
            return Err(ApproveJoinError::InvalidToken.into());
        }
        let pre_auth = group
            .pending_joins
            .iter()
            .find(|r| r.id == modify_request_id)
            .and_then(|r| r.pre_auth_token.clone())
            .unwrap_or_default();
        crate::group::approve_join_request(group, &modify_request_id, &pre_auth).map_err(|e| {
            let msg = e.to_string();
            if msg.contains("not found") {
                ApproveJoinError::RequestNotFound.into()
            } else {
                ApproveJoinError::BadRequest(msg).into()
            }
        })
    })
    .await
    .map_err(|e| {
        if is_not_found(&e) {
            return (StatusCode::NOT_FOUND, "group not found".to_string());
        }
        if let Some(ae) = e.downcast_ref::<ApproveJoinError>() {
            match ae {
                ApproveJoinError::InvalidToken => (StatusCode::UNAUTHORIZED, ae.to_string()),
                ApproveJoinError::RequestNotFound => (StatusCode::NOT_FOUND, ae.to_string()),
                ApproveJoinError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            }
        } else {
            warn!("approve join request failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to approve join request".to_string(),
            )
        }
    })?;

    Ok(Json(crate::group::JoinResult {
        id: request_id,
        status: "approved".to_string(),
        name,
        github: None,
        // The approved participant claims this token with its one-time
        // pre-authorization secret. Do not disclose it to the approver.
        member_token: None,
        pre_auth_token: None,
    }))
}

async fn group_reject_join_handler(
    Path((id, request_id)): Path<(String, String)>,
    Query(_query): Query<GroupTokenQuery>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<crate::group::JoinResult>, (StatusCode, String)> {
    if let Err(message) =
        check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        return Err((StatusCode::TOO_MANY_REQUESTS, message.to_string()));
    }
    check_origin(&state.allowed_origins, &headers)
        .map_err(|error| (StatusCode::FORBIDDEN, error.to_string()))?;
    crate::threads::validate_id(&id)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    crate::threads::validate_id(&request_id)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let token = extract_group_token_from_headers(&headers);
    let modify_request_id = request_id.clone();
    let name = crate::group::modify_group_async(&id, move |group| {
        if !crate::group::is_host_member_token(group, &token) {
            return Err(ApproveJoinError::InvalidToken.into());
        }
        crate::group::reject_join_request(group, &modify_request_id).map_err(|error| {
            let message = error.to_string();
            if message.contains("not found") {
                ApproveJoinError::RequestNotFound.into()
            } else {
                ApproveJoinError::BadRequest(message).into()
            }
        })
    })
    .await
    .map_err(|error| {
        if is_not_found(&error) {
            return (StatusCode::NOT_FOUND, "group not found".to_string());
        }
        if let Some(rejection) = error.downcast_ref::<ApproveJoinError>() {
            return match rejection {
                ApproveJoinError::InvalidToken => (StatusCode::UNAUTHORIZED, rejection.to_string()),
                ApproveJoinError::RequestNotFound => (StatusCode::NOT_FOUND, rejection.to_string()),
                ApproveJoinError::BadRequest(message) => (StatusCode::BAD_REQUEST, message.clone()),
            };
        }
        warn!("reject join request failed: {error}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to reject join request".to_string(),
        )
    })?;
    Ok(Json(crate::group::JoinResult {
        id: request_id,
        status: "rejected".to_string(),
        name,
        github: None,
        member_token: None,
        pre_auth_token: None,
    }))
}

fn extract_pre_auth(headers: &HeaderMap) -> Option<String> {
    if let Some(t) = headers
        .get("x-pre-auth-token")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
    {
        return Some(t.to_string());
    }
    if let Some(t) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| !v.is_empty())
    {
        return Some(t.to_string());
    }
    None
}

async fn group_join_status_handler(
    Path((id, request_id)): Path<(String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<crate::group::JoinResult>, (StatusCode, String)> {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, msg);
        return Err((StatusCode::TOO_MANY_REQUESTS, msg.to_string()));
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, msg);
        return Err((StatusCode::FORBIDDEN, msg.to_string()));
    }
    if let Err(e) = crate::threads::validate_id(&id) {
        return Err((StatusCode::BAD_REQUEST, e.to_string()));
    }
    if let Err(e) = crate::threads::validate_id(&request_id) {
        return Err((StatusCode::BAD_REQUEST, e.to_string()));
    }

    let pre_auth = extract_pre_auth(&headers).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            "missing pre-auth token".to_string(),
        )
    })?;

    let group = crate::group::load_group_async(&id)
        .await
        .map_err(map_join_status_error)?;
    for r in &group.pending_joins {
        if r.id == request_id
            && r.pre_auth_token
                .as_deref()
                .is_some_and(|t| crate::group::constant_time_token_eq(t, &pre_auth))
        {
            return Ok(Json(crate::group::JoinResult {
                id: request_id,
                status: "pending".to_string(),
                name: r.name.clone(),
                github: r.github.clone(),
                member_token: None,
                pre_auth_token: None,
            }));
        }
    }

    let claim_pre_auth = pre_auth.clone();
    let claim_request_id = request_id.clone();
    let (name, token) = crate::group::modify_group_async(&id, move |group| {
        crate::group::lease_approved_member_claim(group, &claim_request_id, &claim_pre_auth)
            .ok_or_else(|| anyhow::anyhow!("join request not found"))
    })
    .await
    .map_err(map_join_status_error)?;
    Ok(Json(crate::group::JoinResult {
        id: request_id,
        status: "approved".to_string(),
        name,
        github: None,
        member_token: Some(token),
        pre_auth_token: None,
    }))
}

fn map_join_status_error(error: anyhow::Error) -> (StatusCode, String) {
    let missing_file = error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    });
    if missing_file || error.to_string() == "join request not found" {
        return (StatusCode::NOT_FOUND, "join request not found".to_string());
    }
    warn!("group join status lookup failed: {error:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "could not read join status; retry without discarding the claim token".to_string(),
    )
}

async fn group_join_ack_handler(
    Path((id, request_id)): Path<(String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<StatusCode, (StatusCode, String)> {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        return Err((StatusCode::TOO_MANY_REQUESTS, msg.to_string()));
    }
    check_origin(&state.allowed_origins, &headers)
        .map_err(|error| (StatusCode::FORBIDDEN, error.to_string()))?;
    crate::threads::validate_id(&id)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    crate::threads::validate_id(&request_id)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?;
    let pre_auth = extract_pre_auth(&headers).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            "missing pre-auth token".to_string(),
        )
    })?;

    crate::group::modify_group_async(&id, move |group| {
        crate::group::acknowledge_join_approval(group, &request_id, &pre_auth)
    })
    .await
    .map_err(|error| {
        warn!("group join acknowledgement failed: {error}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to acknowledge join approval".to_string(),
        )
    })?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RemoteAgentMessagePayload {
    content: String,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    reply_to: Option<String>,
}

fn required_remote_agent_message_id(
    value: Option<&str>,
) -> std::result::Result<String, &'static str> {
    value
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .ok_or("message_id is required for idempotent agent delivery")
}

async fn group_remote_agent_message_handler(
    Path((id, name)): Path<(String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<RemoteAgentMessagePayload>,
) -> Result<StatusCode, (StatusCode, String)> {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, msg);
        return Err((StatusCode::TOO_MANY_REQUESTS, msg.to_string()));
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, msg);
        return Err((StatusCode::FORBIDDEN, msg.to_string()));
    }

    if let Err(e) = crate::threads::validate_id(&id) {
        return Err((StatusCode::BAD_REQUEST, e.to_string()));
    }
    if let Err(e) = crate::group::validate_human_name(&name) {
        return Err((StatusCode::BAD_REQUEST, e.to_string()));
    }

    let token = headers
        .get("x-agent-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let content = payload.content.trim().to_string();
    if content.len() > crate::group::MAX_GROUP_MESSAGE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "message too large".to_string(),
        ));
    }

    let name_for_modify = name.clone();
    let agent_name = crate::group::modify_group_async(&id, move |group| {
        let remote = group
            .remote_agents
            .iter_mut()
            .find(|r| r.name.eq_ignore_ascii_case(&name_for_modify))
            .ok_or_else(|| anyhow::anyhow!("remote agent not found"))?;
        if !crate::group::constant_time_token_eq(&token, &remote.token) {
            bail!("invalid agent token");
        }
        remote.last_heartbeat = Some(Utc::now());
        Ok(remote.name.clone())
    })
    .await
    .map_err(|e| {
        let msg = e.to_string();
        if msg.contains("not found") {
            (StatusCode::NOT_FOUND, msg)
        } else if msg.contains("invalid agent token") {
            (StatusCode::UNAUTHORIZED, msg)
        } else {
            (StatusCode::INTERNAL_SERVER_ERROR, msg)
        }
    })?;

    if content.is_empty() || content.eq_ignore_ascii_case("NO_REPLY") {
        return Ok(StatusCode::NO_CONTENT);
    }

    let message_id = required_remote_agent_message_id(payload.message_id.as_deref())
        .map_err(|message| (StatusCode::BAD_REQUEST, message.to_string()))?;
    if crate::threads::validate_id(&message_id).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid agent message id".to_string(),
        ));
    }
    let reply_to = payload
        .reply_to
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    if reply_to
        .as_deref()
        .is_some_and(|id| crate::threads::validate_id(id).is_err())
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid reply target id".to_string(),
        ));
    }

    let message = crate::group::GroupMessage {
        id: message_id.clone(),
        timestamp: Utc::now(),
        sender: agent_name.clone(),
        content,
        kind: crate::group::MessageKind::Agent,
        client_message_id: Some(message_id),
        reply_to,
    };
    crate::group::persist_and_queue_message_async(&id, &message, &agent_name)
        .await
        .map_err(|error| {
            if error.to_string().contains("different payload") {
                (
                    StatusCode::CONFLICT,
                    "agent message id is already associated with different content".to_string(),
                )
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "failed to save and queue remote agent message".to_string(),
                )
            }
        })?;

    if let Err(error) = crate::group::schedule_group_dispatch(id.clone()) {
        eprintln!("warning: remote message is durable but dispatch scheduling failed: {error}");
    }

    Ok(StatusCode::CREATED)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostedDispatchCacheEntry {
    dispatch_id: String,
    group_id: String,
    agent_name: String,
    payload_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    response: Option<crate::group::RemoteAgentDispatchResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HostedDispatchTombstone {
    dispatch_id: String,
    group_id: String,
    agent_name: String,
    payload_hash: String,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HostedDispatchCache {
    #[serde(default)]
    records: Vec<HostedDispatchCacheEntry>,
    #[serde(default)]
    tombstones: Vec<HostedDispatchTombstone>,
}

enum HostedDispatchAdmission {
    Start,
    Ready(crate::group::RemoteAgentDispatchResponse),
    Completed,
    Ambiguous,
}

fn hosted_dispatch_cache_path() -> Result<std::path::PathBuf> {
    Ok(omg_dir()?.join("hosted_dispatch_cache.json"))
}

fn hosted_dispatch_cache_lock() -> Result<std::fs::File> {
    let dir = omg_dir()?;
    std::fs::create_dir_all(&dir)?;
    crate::providers::restrict_omg_directory_permissions(&dir)?;
    let path = dir.join("hosted_dispatch_cache.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    crate::providers::restrict_omg_file_permissions(&path)?;
    file.lock_exclusive()?;
    Ok(file)
}

fn open_hosted_dispatch_attempt_lock(dispatch_id: &str) -> Result<std::fs::File> {
    crate::threads::validate_id(dispatch_id)?;
    let dir = omg_dir()?.join("hosted-dispatch-attempts");
    std::fs::create_dir_all(&dir)?;
    crate::providers::restrict_omg_directory_permissions(&dir)?;
    let path = dir.join(format!("{dispatch_id}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    crate::providers::restrict_omg_file_permissions(&path)?;
    Ok(file)
}

fn hosted_dispatch_attempt_lock(dispatch_id: &str) -> Result<std::fs::File> {
    let file = open_hosted_dispatch_attempt_lock(dispatch_id)?;
    file.lock_exclusive()?;
    Ok(file)
}

fn try_hosted_dispatch_attempt_lock(dispatch_id: &str) -> Result<Option<std::fs::File>> {
    let file = open_hosted_dispatch_attempt_lock(dispatch_id)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn load_hosted_dispatch_cache() -> Result<HostedDispatchCache> {
    let path = hosted_dispatch_cache_path()?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => bail!("hosted dispatch cache is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HostedDispatchCache::default());
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > MAX_HOSTED_DISPATCH_CACHE_BYTES {
        bail!("hosted dispatch cache exceeds its byte limit");
    }
    let cache: HostedDispatchCache =
        serde_json::from_slice(&std::fs::read(&path)?).context("parse hosted dispatch cache")?;
    if cache.records.len() > MAX_HOSTED_DISPATCH_CACHE_RECORDS {
        bail!("hosted dispatch cache exceeds its record limit");
    }
    if cache.tombstones.len() > MAX_HOSTED_DISPATCH_TOMBSTONES {
        bail!("hosted dispatch cache exceeds its tombstone limit");
    }
    let mut ids = std::collections::HashSet::new();
    for record in &cache.records {
        crate::threads::validate_id(&record.dispatch_id)?;
        crate::threads::validate_id(&record.group_id)?;
        crate::group::validate_human_name(&record.agent_name)?;
        if record.payload_hash.len() != 64
            || !record
                .payload_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || record.response.as_ref().is_some_and(|response| {
                response.content.len() > crate::group::MAX_GROUP_MESSAGE_BYTES
            })
            || record
                .failure
                .as_ref()
                .is_some_and(|failure| failure.len() > MAX_HOSTED_DISPATCH_FAILURE_BYTES)
            || !ids.insert(record.dispatch_id.as_str())
        {
            bail!("hosted dispatch cache contains an invalid record");
        }
    }
    for record in &cache.tombstones {
        crate::threads::validate_id(&record.dispatch_id)?;
        crate::threads::validate_id(&record.group_id)?;
        crate::group::validate_human_name(&record.agent_name)?;
        if record.payload_hash.len() != 64
            || !record
                .payload_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || !ids.insert(record.dispatch_id.as_str())
        {
            bail!("hosted dispatch cache contains an invalid tombstone");
        }
    }
    Ok(cache)
}

fn save_hosted_dispatch_cache(cache: &HostedDispatchCache) -> Result<()> {
    if cache.records.len() > MAX_HOSTED_DISPATCH_CACHE_RECORDS {
        bail!("hosted dispatch cache exceeds its record limit");
    }
    if cache.tombstones.len() > MAX_HOSTED_DISPATCH_TOMBSTONES {
        bail!("hosted dispatch cache exceeds its tombstone limit");
    }
    let bytes = serde_json::to_vec_pretty(cache)?;
    if bytes.len() as u64 > MAX_HOSTED_DISPATCH_CACHE_BYTES {
        bail!("hosted dispatch cache exceeds its byte limit");
    }
    crate::providers::write_file_atomic(&hosted_dispatch_cache_path()?, bytes, true)
}

fn compact_one_completed_hosted_dispatch(
    cache: &mut HostedDispatchCache,
    protected_dispatch_id: Option<&str>,
) -> Result<bool> {
    let index = cache
        .records
        .iter()
        .position(|record| {
            record.response.is_some() && protected_dispatch_id != Some(record.dispatch_id.as_str())
        })
        .or_else(|| {
            cache
                .records
                .iter()
                .position(|record| record.response.is_some())
        });
    let Some(index) = index else {
        return Ok(false);
    };
    if cache.tombstones.len() >= MAX_HOSTED_DISPATCH_TOMBSTONES {
        bail!(
            "hosted dispatch completion tombstones are full; refusing to erase idempotency evidence"
        );
    }
    let record = cache.records.remove(index);
    cache.tombstones.push(HostedDispatchTombstone {
        dispatch_id: record.dispatch_id,
        group_id: record.group_id,
        agent_name: record.agent_name,
        payload_hash: record.payload_hash,
        updated_at: record.updated_at,
    });
    Ok(true)
}

fn save_hosted_dispatch_cache_compacting(
    cache: &mut HostedDispatchCache,
    protected_dispatch_id: Option<&str>,
) -> Result<()> {
    loop {
        let encoded = serde_json::to_vec_pretty(&*cache)?;
        let record_excess = cache
            .records
            .len()
            .saturating_sub(MAX_HOSTED_DISPATCH_CACHE_RECORDS);
        let byte_excess = encoded
            .len()
            .saturating_sub(MAX_HOSTED_DISPATCH_CACHE_BYTES as usize);
        if record_excess == 0 && byte_excess == 0 {
            return crate::providers::write_file_atomic(
                &hosted_dispatch_cache_path()?,
                encoded,
                true,
            );
        }
        let average_record_bytes = encoded.len() / cache.records.len().max(1);
        let byte_estimate = if byte_excess == 0 {
            0
        } else {
            byte_excess.div_ceil(average_record_bytes.max(1))
        };
        let batch = record_excess.max(byte_estimate).clamp(1, 64);
        let mut compacted = 0;
        for _ in 0..batch {
            if !compact_one_completed_hosted_dispatch(cache, protected_dispatch_id)? {
                break;
            }
            compacted += 1;
        }
        if compacted == 0 {
            bail!("hosted dispatch cache has too many unresolved records");
        }
    }
}

fn hosted_dispatch_payload_matches(
    group_id: &str,
    agent_name: &str,
    payload_hash: &str,
    record_group_id: &str,
    record_agent_name: &str,
    record_payload_hash: &str,
) -> bool {
    record_group_id == group_id
        && record_agent_name.eq_ignore_ascii_case(agent_name)
        && record_payload_hash == payload_hash
}

fn begin_hosted_dispatch(
    dispatch_id: &str,
    group_id: &str,
    agent_name: &str,
    payload_hash: &str,
) -> Result<HostedDispatchAdmission> {
    crate::threads::validate_id(dispatch_id)?;
    crate::threads::validate_id(group_id)?;
    crate::group::validate_human_name(agent_name)?;
    let _lock = hosted_dispatch_cache_lock()?;
    let mut cache = load_hosted_dispatch_cache()?;
    if let Some(record) = cache
        .records
        .iter()
        .find(|record| record.dispatch_id == dispatch_id)
    {
        if !hosted_dispatch_payload_matches(
            group_id,
            agent_name,
            payload_hash,
            &record.group_id,
            &record.agent_name,
            &record.payload_hash,
        ) {
            bail!("hosted dispatch id is already associated with a different payload");
        }
        return Ok(match &record.response {
            Some(response) => HostedDispatchAdmission::Ready(response.clone()),
            None => HostedDispatchAdmission::Ambiguous,
        });
    }
    if let Some(record) = cache
        .tombstones
        .iter()
        .find(|record| record.dispatch_id == dispatch_id)
    {
        if !hosted_dispatch_payload_matches(
            group_id,
            agent_name,
            payload_hash,
            &record.group_id,
            &record.agent_name,
            &record.payload_hash,
        ) {
            bail!("hosted dispatch id is already associated with a different payload");
        }
        return Ok(HostedDispatchAdmission::Completed);
    }
    cache.records.push(HostedDispatchCacheEntry {
        dispatch_id: dispatch_id.to_string(),
        group_id: group_id.to_string(),
        agent_name: agent_name.to_string(),
        payload_hash: payload_hash.to_string(),
        response: None,
        failure: None,
        updated_at: Utc::now(),
    });
    save_hosted_dispatch_cache_compacting(&mut cache, None)?;
    Ok(HostedDispatchAdmission::Start)
}

fn finish_hosted_dispatch(
    dispatch_id: &str,
    response: &crate::group::RemoteAgentDispatchResponse,
) -> Result<()> {
    let _lock = hosted_dispatch_cache_lock()?;
    let mut cache = load_hosted_dispatch_cache()?;
    let record = cache
        .records
        .iter_mut()
        .find(|record| record.dispatch_id == dispatch_id)
        .ok_or_else(|| anyhow::anyhow!("hosted dispatch cache record disappeared"))?;
    record.response = Some(response.clone());
    record.failure = None;
    record.updated_at = Utc::now();
    save_hosted_dispatch_cache_compacting(&mut cache, Some(dispatch_id))
}

fn truncate_hosted_dispatch_failure(failure: &str) -> String {
    let mut end = failure.len().min(MAX_HOSTED_DISPATCH_FAILURE_BYTES);
    while !failure.is_char_boundary(end) {
        end -= 1;
    }
    failure[..end].to_string()
}

fn fail_hosted_dispatch(dispatch_id: &str, failure: &str) -> Result<()> {
    let _lock = hosted_dispatch_cache_lock()?;
    let mut cache = load_hosted_dispatch_cache()?;
    let record = cache
        .records
        .iter_mut()
        .find(|record| record.dispatch_id == dispatch_id)
        .ok_or_else(|| anyhow::anyhow!("hosted dispatch cache record disappeared"))?;
    if record.response.is_some() {
        bail!("completed hosted dispatch cannot be marked failed");
    }
    record.failure = Some(truncate_hosted_dispatch_failure(failure));
    record.updated_at = Utc::now();
    save_hosted_dispatch_cache_compacting(&mut cache, None)
}

pub(crate) fn retire_hosted_dispatch(dispatch_id: &str) -> Result<bool> {
    crate::threads::validate_id(dispatch_id)?;
    let Some(_attempt_lock) = try_hosted_dispatch_attempt_lock(dispatch_id)? else {
        bail!("hosted dispatch is still executing and cannot be retired");
    };
    let _lock = hosted_dispatch_cache_lock()?;
    let mut cache = load_hosted_dispatch_cache()?;
    if cache
        .tombstones
        .iter()
        .any(|record| record.dispatch_id == dispatch_id)
    {
        bail!("completed hosted dispatches cannot be retired");
    }
    let Some(index) = cache
        .records
        .iter()
        .position(|record| record.dispatch_id == dispatch_id)
    else {
        return Ok(false);
    };
    if cache.records[index].response.is_some() {
        bail!("completed hosted dispatches cannot be retired");
    }
    cache.records.remove(index);
    save_hosted_dispatch_cache(&cache)?;
    Ok(true)
}

async fn hosted_dispatch_gate(state: &ProxyState, dispatch_id: &str) -> Result<Arc<Mutex<()>>> {
    let mut gates = state.hosted_dispatch_gates.lock().await;
    if gates.len() >= MAX_HOSTED_DISPATCH_GATES && !gates.contains_key(dispatch_id) {
        gates.retain(|_, gate| Arc::strong_count(gate) > 1);
        if gates.len() >= MAX_HOSTED_DISPATCH_GATES {
            bail!("too many active hosted dispatches");
        }
    }
    Ok(gates
        .entry(dispatch_id.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone())
}

async fn group_remote_agent_dispatch_handler(
    Path((id, name)): Path<(String, String)>,
    headers: HeaderMap,
    State(state): State<Arc<ProxyState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(payload): Json<crate::group::RemoteAgentDispatchPayload>,
) -> Result<Json<crate::group::RemoteAgentDispatchResponse>, (StatusCode, String)> {
    if let Err(msg) = check_rate_limit(state.rate_limit_per_minute, &state.rate_limiter, addr).await
    {
        warn!("Rate limit exceeded for {}: {}", addr, msg);
        return Err((StatusCode::TOO_MANY_REQUESTS, msg.to_string()));
    }
    if let Err(msg) = check_origin(&state.allowed_origins, &headers) {
        warn!("Origin check failed for {}: {}", addr, msg);
        return Err((StatusCode::FORBIDDEN, msg.to_string()));
    }

    if let Err(e) = crate::threads::validate_id(&id) {
        return Err((StatusCode::BAD_REQUEST, e.to_string()));
    }
    if let Err(e) = crate::group::validate_human_name(&name) {
        return Err((StatusCode::BAD_REQUEST, e.to_string()));
    }

    let token = headers
        .get("x-agent-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(allow_yolo) = crate::group::hosted_agent_yolo_authorization(&id, &name, token) else {
        return Err((StatusCode::UNAUTHORIZED, "invalid agent token".to_string()));
    };
    if payload.yolo && !allow_yolo {
        return Err((
            StatusCode::FORBIDDEN,
            "hosted agent is not authorized for yolo execution".to_string(),
        ));
    }
    if payload.group_id != id || !payload.agent_name.eq_ignore_ascii_case(&name) {
        return Err((
            StatusCode::BAD_REQUEST,
            "payload group/agent mismatch".to_string(),
        ));
    }
    if crate::threads::validate_id(&payload.dispatch_id).is_err() {
        return Err((
            StatusCode::BAD_REQUEST,
            "invalid remote dispatch id".to_string(),
        ));
    }
    if payload.prompt.len() > MAX_REMOTE_AGENT_PROMPT_BYTES
        || payload.model.len() > 256
        || payload.group_model.len() > 256
        || payload.history.len() > 50
    {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "remote agent dispatch payload is too large".to_string(),
        ));
    }

    let payload_hash = blake3::hash(
        &serde_json::to_vec(&payload)
            .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))?,
    )
    .to_hex()
    .to_string();
    let dispatch_id = payload.dispatch_id.clone();
    let gate = hosted_dispatch_gate(&state, &dispatch_id)
        .await
        .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    let _dispatch_guard = gate.lock().await;
    let attempt_dispatch_id = dispatch_id.clone();
    let _attempt_guard =
        tokio::task::spawn_blocking(move || hosted_dispatch_attempt_lock(&attempt_dispatch_id))
            .await
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
            .map_err(|error| (StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    let admission = {
        let dispatch_id = dispatch_id.clone();
        let id = id.clone();
        let name = name.clone();
        tokio::task::spawn_blocking(move || {
            begin_hosted_dispatch(&dispatch_id, &id, &name, &payload_hash)
        })
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
        .map_err(|error| {
            let status = if error.to_string().contains("different payload") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status, error.to_string())
        })?
    };
    match admission {
        HostedDispatchAdmission::Ready(response) => return Ok(Json(response)),
        HostedDispatchAdmission::Completed => {
            return Err((
                StatusCode::CONFLICT,
                "remote dispatch already completed; its exact cached response expired, and inference will not be repeated"
                    .to_string(),
            ));
        }
        HostedDispatchAdmission::Ambiguous => {
            return Err((
                StatusCode::CONFLICT,
                "remote dispatch has an unresolved prior attempt; refusing to repeat inference"
                    .to_string(),
            ));
        }
        HostedDispatchAdmission::Start => {}
    }

    let model = crate::group::normalize_model(&payload.model);
    let model = if model.is_empty() {
        if payload.group_model.is_empty() {
            None
        } else {
            Some(crate::group::normalize_model(&payload.group_model))
        }
    } else {
        Some(model)
    };
    let yolo = payload.yolo && allow_yolo;
    let prompt = payload.prompt;
    let tools = yolo.then(|| crate::all_tool_ids_csv().clone());
    let max_turns = if yolo { Some(8) } else { Some(1) };
    let result = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(crate::run_single_turn_capture(
            &prompt, model, yolo, max_turns, tools,
        ))
    })
    .await;
    let content = match result {
        Ok(Ok(content)) => content,
        Ok(Err(error)) => {
            let message = error.to_string();
            let failure_dispatch_id = dispatch_id.clone();
            let failure_message = message.clone();
            let persisted = tokio::task::spawn_blocking(move || {
                fail_hosted_dispatch(&failure_dispatch_id, &failure_message)
            })
            .await
            .map_err(|join_error| (StatusCode::INTERNAL_SERVER_ERROR, join_error.to_string()))?;
            if let Err(persist_error) = persisted {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "remote inference failed: {message}; preserving its ambiguous admission also failed: {persist_error}"
                    ),
                ));
            }
            return Err((StatusCode::BAD_REQUEST, message));
        }
        Err(error) => {
            let message = error.to_string();
            let failure_dispatch_id = dispatch_id.clone();
            let failure_message = message.clone();
            let persisted = tokio::task::spawn_blocking(move || {
                fail_hosted_dispatch(&failure_dispatch_id, &failure_message)
            })
            .await
            .map_err(|join_error| (StatusCode::INTERNAL_SERVER_ERROR, join_error.to_string()))?;
            if let Err(persist_error) = persisted {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "remote inference task failed: {message}; preserving its ambiguous admission also failed: {persist_error}"
                    ),
                ));
            }
            return Err((StatusCode::INTERNAL_SERVER_ERROR, message));
        }
    };

    let response = crate::group::RemoteAgentDispatchResponse {
        content: crate::group::truncate_message_content(&content)
            .trim()
            .to_string(),
    };
    let response_to_save = response.clone();
    let dispatch_id_to_save = dispatch_id.clone();
    tokio::task::spawn_blocking(move || {
        finish_hosted_dispatch(&dispatch_id_to_save, &response_to_save)
    })
    .await
    .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?
    .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    Ok(Json(response))
}

pub async fn serve(args: &ServeArgs) -> Result<()> {
    let mut agent_config = crate::build_agent_config(args.model.clone())?;
    agent_config.default_yolo_mode = args.yolo;
    let voice_config = relay_voice_config(&agent_config);
    let voice_auth =
        xai_grok_pager::voice::build_voice_auth(Arc::new(agent_config.create_auth_manager()));

    let (public_secret, secret_path, provided) = match &args.secret {
        Some(s) => {
            if !is_valid_pairing_secret(s) {
                bail!(
                    "provided secret must contain 16-{MAX_PAIRING_SECRET_BYTES} ASCII WebSocket-token characters"
                );
            }
            (s.clone(), None, true)
        }
        None => {
            let dir = omg_dir()?;
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("serve.secret");
            if let Some(s) = read_persisted_secret(&path) {
                (s, Some(path), false)
            } else {
                let s = generate_secret()?;
                crate::providers::write_file_atomic(&path, &s, true)?;
                (s, Some(path), false)
            }
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
            "warning: omgb serve is listening on a non-loopback address and the pairing URL uses plaintext ws://; use a TLS-terminating reverse proxy if you need wss://"
        );
    }
    let cors = cors_layer(&allowed_origins)?;

    let upstream_secret = generate_secret()?;
    let mut upstream_agent = spawn_upstream_agent(agent_config, &upstream_secret).await?;

    let secret_hash = *blake3::hash(public_secret.as_bytes()).as_bytes();
    let upstream_url = format!("ws://127.0.0.1:{}/ws", upstream_agent.addr.port());
    let state = Arc::new(ProxyState {
        secret_hash,
        allowed_origins,
        rate_limit_per_minute,
        rate_limiter: Arc::new(Mutex::new(HashMap::new())),
        connection_limit: Arc::new(Semaphore::new(MAX_ACTIVE_PROXY_CONNECTIONS)),
        upstream_url,
        upstream_secret,
        voice_config,
        voice_auth,
        hosted_dispatch_gates: Arc::new(Mutex::new(HashMap::new())),
        started_at: Instant::now(),
    });

    let rate_limiter_task = tokio::spawn(cleanup_rate_limiter(
        state.rate_limit_per_minute,
        state.rate_limiter.clone(),
    ));

    let app = Router::new()
        .route("/healthz", get(health_handler))
        .route("/capabilities", get(capabilities_handler))
        .route("/status", get(relay_status_handler))
        .route("/ws", get(ws_handler))
        .route("/acp", get(ws_handler))
        .route("/voice", get(voice_ws_handler))
        .route("/group", post(admin_create_group_handler))
        .route("/group/{id}", get(group_info_handler))
        .route("/group/{id}/joins", get(group_list_joins_handler))
        .route("/group/{id}/join", post(group_request_join_handler))
        .route(
            "/group/{id}/joins/{request_id}/approve",
            post(group_approve_join_handler),
        )
        .route(
            "/group/{id}/joins/{request_id}/reject",
            post(group_reject_join_handler),
        )
        .route(
            "/group/{id}/joins/{request_id}/status",
            get(group_join_status_handler),
        )
        .route(
            "/group/{id}/joins/{request_id}/status/ack",
            post(group_join_ack_handler),
        )
        .route(
            "/group/{id}/messages",
            get(group_list_messages_handler).post(group_post_message_handler),
        )
        .route(
            "/group/{id}/dispatch/{trigger_id}",
            get(group_dispatch_status_handler).post(group_dispatch_retry_handler),
        )
        .route(
            "/group/{id}/dispatches",
            get(group_dispatch_statuses_handler),
        )
        .route(
            "/group/{id}/agent/{name}/message",
            post(group_remote_agent_message_handler),
        )
        .route(
            "/group/{id}/agent/{name}/dispatch",
            post(group_remote_agent_dispatch_handler),
        )
        .route("/workflow", post(admin_create_workflow_handler))
        .with_state(state);
    let app = match cors {
        Some(cors) => app.layer(cors),
        None => app,
    };
    let listener = TcpListener::bind(bind_addr).await?;
    let actual_addr = listener.local_addr()?;
    let recovered_dispatches = crate::group::recover_pending_dispatches().await?;
    if recovered_dispatches > 0 {
        println!("  recovered group dispatch queues: {recovered_dispatches}");
    }

    println!("oh-my-grok-build serve");
    println!("  bind: {actual_addr}");
    if let Some(ip) = advertise_host {
        println!("  advertise host: {ip}");
    }
    if let Some(port) = args.advertise_port {
        println!("  advertise port: {port}");
    }
    if let Some(path) = &secret_path {
        println!("  secret file: {}", path.display());
    } else if provided {
        println!("  secret: <provided>");
    }
    print_pairing_info(
        actual_addr,
        &public_secret,
        advertise_host,
        args.advertise_port,
        args.wss,
    );

    let relay = async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    };
    tokio::pin!(relay);
    let result = tokio::select! {
        relay_result = &mut relay => relay_result.context("relay server failed").map(|_| ()),
        upstream_result = &mut upstream_agent.task => upstream_exit_result(upstream_result),
    };
    rate_limiter_task.abort();
    result
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

    let secret = take_pairing_secret(&mut url, args.secret.clone());
    if secret.is_none() {
        anyhow::bail!(
            "--secret is required; use the secret file printed by `omgb serve` or the server-key query parameter"
        );
    }
    if !secret.as_deref().is_some_and(is_valid_pairing_secret) {
        anyhow::bail!(
            "pairing secret must contain 16-{MAX_PAIRING_SECRET_BYTES} ASCII WebSocket-token characters"
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
    fn remote_agent_messages_require_a_stable_id() {
        assert!(required_remote_agent_message_id(None).is_err());
        assert!(required_remote_agent_message_id(Some("  ")).is_err());
        assert_eq!(
            required_remote_agent_message_id(Some(" agent-message-1 ")).unwrap(),
            "agent-message-1"
        );
    }

    #[test]
    fn test_generate_secret_length() {
        let s = generate_secret().unwrap();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generated_pairing_secrets_are_not_reused() {
        assert_ne!(generate_secret().unwrap(), generate_secret().unwrap());
    }

    #[tokio::test]
    async fn health_endpoint_identifies_the_live_relay_without_secrets() {
        let health = health_handler().await.0;
        assert_eq!(health.status, "ok");
        assert_eq!(health.service, "oh-my-grok-build-relay");
        assert_eq!(health.version, env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn capabilities_endpoint_versions_the_mobile_contract_and_limits() {
        let response = capabilities_handler().await.0;
        assert_eq!(response.service, "oh-my-grok-build-relay");
        assert_eq!(response.api_version, RELAY_API_VERSION);
        assert!(response.capabilities.contains(&"acp.session.resume"));
        assert!(response.capabilities.contains(&"group.join.ack.v1"));
        assert!(response.capabilities.contains(&"relay.status.v1"));
        assert_eq!(
            response.limits.group_message_bytes,
            crate::group::MAX_GROUP_MESSAGE_BYTES
        );
        assert_eq!(response.limits.group_message_page, MAX_GROUP_MESSAGE_PAGE);
    }

    #[tokio::test]
    async fn relay_status_is_authenticated_and_contains_only_bounded_operational_data() {
        let state = test_state("operator-secret");
        let address: SocketAddr = "127.0.0.1:41234".parse().unwrap();
        let unauthorized =
            relay_status_handler(HeaderMap::new(), State(state.clone()), ConnectInfo(address))
                .await
                .unwrap_err();
        assert_eq!(unauthorized.0, StatusCode::UNAUTHORIZED);

        let _connection = state.connection_limit.acquire().await.unwrap();
        let dispatch_gate = Arc::new(Mutex::new(()));
        state
            .hosted_dispatch_gates
            .lock()
            .await
            .insert("dispatch-1".into(), dispatch_gate.clone());
        let mut headers = HeaderMap::new();
        headers.insert("x-server-token", "operator-secret".parse().unwrap());
        let status = relay_status_handler(headers, State(state.clone()), ConnectInfo(address))
            .await
            .unwrap()
            .0;
        assert_eq!(status.status, "ready");
        assert_eq!(status.api_version, RELAY_API_VERSION);
        assert_eq!(status.active_connections, 1);
        assert_eq!(status.connection_limit, MAX_ACTIVE_PROXY_CONNECTIONS);
        assert_eq!(status.local_hosted_dispatches, 1);
        assert_eq!(status.rate_limit_per_minute, None);

        let encoded = serde_json::to_string(&status).unwrap();
        assert!(!encoded.contains("operator-secret"));
        assert!(!encoded.contains("upstream_secret"));
        assert!(!encoded.contains("group"));
    }

    #[tokio::test]
    async fn upstream_exit_is_always_a_relay_failure() {
        let clean_exit = upstream_exit_result(Ok(Ok(()))).unwrap_err();
        assert!(clean_exit.to_string().contains("exited unexpectedly"));

        let failed_exit =
            upstream_exit_result(Ok(Err(anyhow::anyhow!("agent crashed")))).unwrap_err();
        assert!(failed_exit.to_string().contains("agent crashed"));

        let task = tokio::spawn(async { std::future::pending::<Result<()>>().await });
        task.abort();
        let cancelled_exit = upstream_exit_result(task.await).unwrap_err();
        assert!(cancelled_exit.to_string().contains("was cancelled"));
    }

    #[test]
    fn pairing_secret_is_safe_for_authorization_and_websocket_protocols() {
        assert!(is_valid_pairing_secret("0123456789abcdef"));
        assert!(is_valid_pairing_secret("safe-token_012345"));
        assert!(!is_valid_pairing_secret("too-short"));
        assert!(!is_valid_pairing_secret("contains a space"));
        assert!(!is_valid_pairing_secret("contains,comma-123"));
    }

    #[test]
    fn join_status_only_reports_not_found_for_terminal_missing_state() {
        let (status, _) = map_join_status_error(anyhow::anyhow!(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "missing group",
        )));
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, message) = map_join_status_error(anyhow::anyhow!("corrupt group JSON"));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(message.contains("retry"));
    }

    #[test]
    fn voice_start_only_accepts_mono_signed_pcm_at_safe_rates() {
        assert_eq!(
            validate_voice_start(VoiceClientMessage::Start {
                sample_rate: 16_000,
                channels: 1,
                encoding: "int16".into(),
            }),
            Ok(16_000)
        );
        for message in [
            VoiceClientMessage::Start {
                sample_rate: 7_999,
                channels: 1,
                encoding: "int16".into(),
            },
            VoiceClientMessage::Start {
                sample_rate: 48_001,
                channels: 1,
                encoding: "int16".into(),
            },
            VoiceClientMessage::Start {
                sample_rate: 16_000,
                channels: 2,
                encoding: "int16".into(),
            },
            VoiceClientMessage::Start {
                sample_rate: 16_000,
                channels: 1,
                encoding: "float32".into(),
            },
            VoiceClientMessage::Stop,
        ] {
            assert!(validate_voice_start(message).is_err());
        }
    }

    #[tokio::test]
    async fn voice_pcm_forwarding_obeys_backpressure_and_session_deadlines() {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(25);
        let result = bounded_voice_pcm_forward(
            deadline,
            Duration::from_secs(1),
            std::future::pending::<std::result::Result<(), ()>>(),
        )
        .await;
        assert_eq!(result, Err(VoicePcmForwardError::SessionDeadline));

        let result = bounded_voice_pcm_forward(
            tokio::time::Instant::now() + Duration::from_secs(1),
            Duration::from_millis(25),
            std::future::pending::<std::result::Result<(), ()>>(),
        )
        .await;
        assert_eq!(result, Err(VoicePcmForwardError::Backpressure));

        let result = bounded_voice_pcm_forward(
            tokio::time::Instant::now() + Duration::from_secs(1),
            Duration::from_millis(25),
            std::future::ready(Err::<(), _>("closed")),
        )
        .await;
        assert_eq!(result, Err(VoicePcmForwardError::Closed));
    }

    #[test]
    fn connect_removes_pairing_secrets_from_the_websocket_url() {
        let mut url =
            Url::parse("ws://192.168.1.2/ws?server-key=0123456789abcdef&server_key=other&keep=yes")
                .unwrap();
        let secret = take_pairing_secret(&mut url, None);

        assert_eq!(secret.as_deref(), Some("0123456789abcdef"));
        assert_eq!(url.as_str(), "ws://192.168.1.2/ws?keep=yes");
    }

    #[tokio::test]
    async fn proxy_bridge_cancels_the_surviving_direction() {
        let finished = tokio::spawn(async {});
        let pending = tokio::spawn(async { std::future::pending::<()>().await });

        tokio::time::timeout(
            Duration::from_millis(100),
            finish_proxy_bridge(finished, pending),
        )
        .await
        .expect("bridge cleanup must not wait for a closed peer forever");
    }

    #[test]
    fn relay_connection_limit_is_positive() {
        const { assert!(MAX_ACTIVE_PROXY_CONNECTIONS > 0) };
    }

    #[test]
    fn hosted_dispatch_cache_deduplicates_and_preserves_ambiguous_attempts() {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "omgb-hosted-dispatch-cache-test-{}",
            uuid::Uuid::new_v4()
        ));
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        let hash = "a".repeat(64);

        assert!(matches!(
            begin_hosted_dispatch("dispatch-1", "group-1", "Alpha", &hash).unwrap(),
            HostedDispatchAdmission::Start
        ));
        assert!(matches!(
            begin_hosted_dispatch("dispatch-1", "group-1", "Alpha", &hash).unwrap(),
            HostedDispatchAdmission::Ambiguous
        ));
        let response = crate::group::RemoteAgentDispatchResponse {
            content: "done".into(),
        };
        finish_hosted_dispatch("dispatch-1", &response).unwrap();
        match begin_hosted_dispatch("dispatch-1", "group-1", "Alpha", &hash).unwrap() {
            HostedDispatchAdmission::Ready(cached) => assert_eq!(cached.content, "done"),
            _ => panic!("completed dispatch should return its cached response"),
        }
        assert!(begin_hosted_dispatch("dispatch-1", "group-1", "Alpha", &"b".repeat(64)).is_err());
        assert!(retire_hosted_dispatch("dispatch-1").is_err());
        assert!(matches!(
            begin_hosted_dispatch("dispatch-1", "group-1", "Alpha", &hash).unwrap(),
            HostedDispatchAdmission::Ready(_)
        ));

        assert!(matches!(
            begin_hosted_dispatch("dispatch-2", "group-1", "Alpha", &hash).unwrap(),
            HostedDispatchAdmission::Start
        ));
        fail_hosted_dispatch("dispatch-2", "provider failed after admission").unwrap();
        assert!(matches!(
            begin_hosted_dispatch("dispatch-2", "group-1", "Alpha", &hash).unwrap(),
            HostedDispatchAdmission::Ambiguous
        ));
        assert!(retire_hosted_dispatch("dispatch-2").unwrap());
        assert!(matches!(
            begin_hosted_dispatch("dispatch-2", "group-1", "Alpha", &hash).unwrap(),
            HostedDispatchAdmission::Start
        ));

        crate::providers::set_omg_home_for_tests(None);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn hosted_dispatch_compaction_preserves_completion_tombstones() {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "omgb-hosted-dispatch-tombstone-test-{}",
            uuid::Uuid::new_v4()
        ));
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        let hash = "a".repeat(64);
        let response = crate::group::RemoteAgentDispatchResponse {
            content: "done".into(),
        };
        let mut cache = HostedDispatchCache::default();
        for index in 0..MAX_HOSTED_DISPATCH_CACHE_RECORDS {
            cache.records.push(HostedDispatchCacheEntry {
                dispatch_id: format!("dispatch-{index}"),
                group_id: "group-1".into(),
                agent_name: "Alpha".into(),
                payload_hash: hash.clone(),
                response: Some(response.clone()),
                failure: None,
                updated_at: Utc::now(),
            });
        }
        save_hosted_dispatch_cache(&cache).unwrap();

        assert!(matches!(
            begin_hosted_dispatch("dispatch-new", "group-1", "Alpha", &hash).unwrap(),
            HostedDispatchAdmission::Start
        ));
        assert!(matches!(
            begin_hosted_dispatch("dispatch-0", "group-1", "Alpha", &hash).unwrap(),
            HostedDispatchAdmission::Completed
        ));
        let cache = load_hosted_dispatch_cache().unwrap();
        assert_eq!(cache.records.len(), MAX_HOSTED_DISPATCH_CACHE_RECORDS);
        assert_eq!(cache.tombstones.len(), 1);

        crate::providers::set_omg_home_for_tests(None);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn hosted_dispatch_retirement_refuses_a_live_attempt_lock() {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "omgb-hosted-dispatch-live-test-{}",
            uuid::Uuid::new_v4()
        ));
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        let hash = "a".repeat(64);
        assert!(matches!(
            begin_hosted_dispatch("dispatch-live", "group-1", "Alpha", &hash).unwrap(),
            HostedDispatchAdmission::Start
        ));
        let attempt_lock = hosted_dispatch_attempt_lock("dispatch-live").unwrap();
        assert!(retire_hosted_dispatch("dispatch-live").is_err());
        drop(attempt_lock);
        assert!(retire_hosted_dispatch("dispatch-live").unwrap());

        crate::providers::set_omg_home_for_tests(None);
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn hosted_dispatch_byte_compaction_keeps_idempotency_evidence() {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "omgb-hosted-dispatch-byte-test-{}",
            uuid::Uuid::new_v4()
        ));
        crate::providers::set_omg_home_for_tests(Some(home.clone()));
        let hash = "a".repeat(64);
        let response = crate::group::RemoteAgentDispatchResponse {
            content: "\0".repeat(crate::group::MAX_GROUP_MESSAGE_BYTES),
        };
        let mut cache = HostedDispatchCache::default();
        for index in 0..512 {
            cache.records.push(HostedDispatchCacheEntry {
                dispatch_id: format!("dispatch-{index}"),
                group_id: "group-1".into(),
                agent_name: "Alpha".into(),
                payload_hash: hash.clone(),
                response: Some(response.clone()),
                failure: None,
                updated_at: Utc::now(),
            });
        }
        save_hosted_dispatch_cache_compacting(&mut cache, Some("dispatch-511")).unwrap();
        assert!(!cache.tombstones.is_empty());
        assert!(matches!(
            begin_hosted_dispatch("dispatch-0", "group-1", "Alpha", &hash).unwrap(),
            HostedDispatchAdmission::Completed
        ));
        assert!(matches!(
            begin_hosted_dispatch("dispatch-511", "group-1", "Alpha", &hash).unwrap(),
            HostedDispatchAdmission::Ready(_)
        ));

        crate::providers::set_omg_home_for_tests(None);
        std::fs::remove_dir_all(home).ok();
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
        assert_eq!(
            pairing_url(bind, host, None, false),
            "ws://192.168.1.2:2419/ws"
        );
    }

    #[test]
    fn test_pairing_url_loopback() {
        let bind = SocketAddr::new("127.0.0.1".parse().unwrap(), 2419);
        assert_eq!(
            pairing_url(bind, None, None, false),
            "ws://127.0.0.1:2419/ws"
        );
    }

    #[test]
    fn test_pairing_url_wss_default_port() {
        let bind = SocketAddr::new("0.0.0.0".parse().unwrap(), 443);
        let host = Some("192.168.1.2".parse().unwrap());
        assert_eq!(pairing_url(bind, host, None, true), "wss://192.168.1.2/ws");
    }

    #[test]
    fn test_pairing_url_wss_advertise_port() {
        let bind = SocketAddr::new("0.0.0.0".parse().unwrap(), 2419);
        let host = Some("192.168.1.2".parse().unwrap());
        assert_eq!(
            pairing_url(bind, host, Some(443), true),
            "wss://192.168.1.2/ws"
        );
    }

    #[test]
    fn test_pairing_payload() {
        let payload = pairing_payload(
            "wss://192.168.1.2:2419/ws",
            "abc123",
            Some(std::path::Path::new("/home/user/project")),
        );
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["url"], "wss://192.168.1.2:2419/ws");
        assert_eq!(parsed["secret"], "abc123");
        assert_eq!(parsed["cwd"], "/home/user/project");
    }

    #[test]
    fn pairing_payload_omits_unsafe_working_directory() {
        let payload = pairing_payload(
            "ws://127.0.0.1:2419/ws",
            "abc123",
            Some(std::path::Path::new("bad\npath")),
        );
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert!(parsed["cwd"].is_null());
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
            connection_limit: Arc::new(Semaphore::new(MAX_ACTIVE_PROXY_CONNECTIONS)),
            upstream_url: String::new(),
            upstream_secret: String::new(),
            voice_config: xai_grok_voice::VoiceConfig::default(),
            voice_auth: xai_grok_voice::StaticVoiceAuth::shared("test-token")
                .expect("test voice token is non-empty"),
            hosted_dispatch_gates: Arc::new(Mutex::new(HashMap::new())),
            started_at: Instant::now(),
        })
    }

    #[test]
    fn cors_is_disabled_without_explicit_origins() {
        let origins: Option<Vec<String>> = None;
        assert!(cors_layer(&origins).unwrap().is_none());
    }

    #[test]
    fn cors_accepts_explicit_origin_or_operator_wildcard() {
        assert!(
            cors_layer(&Some(vec!["https://app.example.test".to_string()]))
                .unwrap()
                .is_some()
        );
        assert!(cors_layer(&Some(vec!["*".to_string()])).unwrap().is_some());
    }

    #[test]
    fn cors_rejects_malformed_configured_origin() {
        assert!(cors_layer(&Some(vec!["not an origin".to_string()])).is_err());
    }

    #[tokio::test]
    async fn test_validate_auth_header() {
        let state = test_state("my-token");
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer my-token".parse().unwrap());
        let query = xai_grok_shell::agent::server::WsQueryParams::default();
        assert_eq!(
            validate_auth(&headers, &query, &state).await.as_deref(),
            Some("my-token")
        );
    }

    #[tokio::test]
    async fn test_validate_auth_protocol() {
        let state = test_state("my-token");
        let mut headers = HeaderMap::new();
        headers.insert("sec-websocket-protocol", "my-token".parse().unwrap());
        let query = xai_grok_shell::agent::server::WsQueryParams::default();
        assert_eq!(
            validate_auth(&headers, &query, &state).await.as_deref(),
            Some("my-token")
        );
    }

    #[tokio::test]
    async fn test_validate_auth_query() {
        let state = test_state("my-token");
        let headers = HeaderMap::new();
        let query = xai_grok_shell::agent::server::WsQueryParams {
            server_key: Some("my-token".into()),
        };
        assert_eq!(
            validate_auth(&headers, &query, &state).await.as_deref(),
            Some("my-token")
        );
    }

    #[tokio::test]
    async fn test_validate_auth_rejects_missing_token() {
        let state = test_state("my-token");
        let headers = HeaderMap::new();
        let query = xai_grok_shell::agent::server::WsQueryParams::default();
        assert!(validate_auth(&headers, &query, &state).await.is_none());
    }

    #[test]
    fn read_persisted_secret_accepts_valid_file() {
        let tmp = std::env::temp_dir().join(format!("omgb-secret-valid-{}", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, "deadbeefcafebabe1122334455667788\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(
            read_persisted_secret(&tmp).unwrap(),
            "deadbeefcafebabe1122334455667788"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn read_persisted_secret_rejects_non_hex() {
        let tmp = std::env::temp_dir().join(format!("omgb-secret-nonhex-{}", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, "not a secret\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(read_persisted_secret(&tmp).is_none());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn read_persisted_secret_rejects_too_large() {
        let tmp = std::env::temp_dir().join(format!("omgb-secret-large-{}", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, "a".repeat(2000)).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(read_persisted_secret(&tmp).is_none());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    #[cfg(unix)]
    fn read_persisted_secret_rejects_world_readable() {
        let tmp = std::env::temp_dir().join(format!("omgb-secret-perm-{}", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, "deadbeefcafebabe1122334455667788\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert!(read_persisted_secret(&tmp).is_none());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    #[cfg(unix)]
    fn read_persisted_secret_rejects_symlink() {
        let tmp =
            std::env::temp_dir().join(format!("omgb-secret-symlink-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let target = tmp.join("target");
        let link = tmp.join("link");
        std::fs::write(&target, "deadbeefcafebabe1122334455667788").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(read_persisted_secret(&link).is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn extract_server_token_uses_headers_only() {
        let mut headers = HeaderMap::new();
        assert_eq!(extract_server_token(&headers), "");
        headers.insert("x-server-token", "server-token".parse().unwrap());
        assert_eq!(extract_server_token(&headers), "server-token");
        headers.insert("authorization", "Bearer bearer-token".parse().unwrap());
        assert_eq!(extract_server_token(&headers), "bearer-token");
    }

    #[test]
    fn extract_group_token_ignores_empty_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-member-token", "".parse().unwrap());
        headers.insert("authorization", "Bearer bearer-token".parse().unwrap());
        let query = GroupTokenQuery { token: None };
        assert_eq!(extract_group_token(&query, &headers), "bearer-token");
    }

    #[test]
    fn extract_group_token_falls_back_to_query() {
        let headers = HeaderMap::new();
        let query = GroupTokenQuery {
            token: Some("query-token".into()),
        };
        assert_eq!(extract_group_token(&query, &headers), "query-token");
    }

    #[test]
    fn member_only_group_token_extraction_never_uses_query() {
        let headers = HeaderMap::new();
        let query = GroupTokenQuery {
            token: Some("query-token".into()),
        };
        assert_eq!(extract_group_token_from_headers(&headers), "");
        assert_eq!(extract_group_token(&query, &headers), "query-token");
    }

    #[test]
    fn extract_pre_auth_from_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer pre-auth-token".parse().unwrap());
        assert_eq!(
            extract_pre_auth(&headers),
            Some("pre-auth-token".to_string())
        );
    }

    #[test]
    fn extract_group_token_skips_empty_bearer() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer ".parse().unwrap());
        let query = GroupTokenQuery {
            token: Some("query-token".into()),
        };
        assert_eq!(extract_group_token(&query, &headers), "query-token");
    }

    #[test]
    fn is_not_found_detects_io_not_found() {
        let path = std::env::temp_dir().join(format!("omgb-missing-{}", uuid::Uuid::new_v4()));
        let err = std::fs::read_to_string(&path)
            .context("read temp")
            .unwrap_err();
        assert!(is_not_found(&err));
    }
}
