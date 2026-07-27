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
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
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
    if s.len() >= 16 && s.chars().all(|c| c.is_ascii_hexdigit()) {
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

fn pairing_payload(url: &str, secret: &str) -> String {
    serde_json::json!({"url": url, "secret": secret }).to_string()
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

    let Some(matched_token) = validate_auth(&headers, &query, &state).await else {
        warn!("Unauthorized connection attempt from {}", addr);
        return (
            StatusCode::UNAUTHORIZED,
            "Invalid or missing authorization token",
        )
            .into_response();
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

#[derive(Deserialize)]
struct GroupTokenQuery {
    token: Option<String>,
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

#[derive(Deserialize)]
struct ServerTokenQuery {
    server_key: Option<String>,
}

fn extract_server_token(query: &ServerTokenQuery, headers: &HeaderMap) -> String {
    if let Some(t) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return t.to_string();
    }
    if let Some(t) = headers.get("x-server-token").and_then(|v| v.to_str().ok()) {
        return t.to_string();
    }
    if let Some(t) = query.server_key.as_deref()
        && !t.is_empty()
    {
        return t.to_string();
    }
    String::new()
}

fn extract_group_token(query: &GroupTokenQuery, headers: &HeaderMap) -> String {
    for header in ["x-member-token", "x-group-token"] {
        if let Some(t) = headers.get(header).and_then(|v| v.to_str().ok()) {
            return t.to_string();
        }
    }
    if let Some(t) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return t.to_string();
    }
    if let Some(t) = query.token.as_deref()
        && !t.is_empty()
    {
        return t.to_string();
    }
    String::new()
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

fn server_token_valid(state: &ProxyState, token: &str) -> bool {
    constant_time_eq::constant_time_eq(
        state.secret_hash.as_ref(),
        blake3::hash(token.as_bytes()).as_bytes(),
    )
}

async fn admin_create_group_handler(
    Query(query): Query<ServerTokenQuery>,
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
    let token = extract_server_token(&query, &headers);
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
    Query(query): Query<ServerTokenQuery>,
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
    let token = extract_server_token(&query, &headers);
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
    let token = extract_group_token(&query, &headers);
    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found"))?;
    if !group_token_valid(&group, &token) {
        return Err((StatusCode::UNAUTHORIZED, "invalid token"));
    }
    let is_member = is_member_token(&group, &token);
    Ok(Json(GroupInfo {
        id: group.id,
        name: group.name,
        description: group.description,
        model: group.model,
        yolo: group.yolo,
        host_name: group.host_name,
        agents: if is_member { group.agents } else { Vec::new() },
        members: if is_member { group.members } else { Vec::new() },
        pending_joins: if is_member {
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
    Query(query): Query<GroupTokenQuery>,
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
    let token = extract_group_token(&query, &headers);
    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found"))?;
    if !is_member_token(&group, &token) {
        return Err((StatusCode::UNAUTHORIZED, "a valid member token is required"));
    }
    let messages = crate::group::load_messages_async(&id)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to load messages"))?;
    Ok(Json(messages))
}

async fn group_post_message_handler(
    Path(id): Path<String>,
    Query(query): Query<GroupTokenQuery>,
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
    let token = extract_group_token(&query, &headers);
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
    if content.len() > crate::group::MAX_GROUP_MESSAGE_BYTES {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            "message too large".to_string(),
        ));
    }
    let message = crate::group::GroupMessage {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        sender,
        content,
        kind: payload.kind,
    };
    crate::group::add_message_async(&id, &message)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to save message".to_string(),
            )
        })?;

    // Trigger agent dispatch off the response path. We run it on a blocking
    // thread with block_on because the upstream headless turn uses non-Send
    // auth state; this also lets the HTTP response return immediately.
    let runtime_handle = tokio::runtime::Handle::current();
    let group_for_dispatch = group.clone();
    let trigger_for_dispatch = message.clone();
    let sender_for_dispatch = message.sender.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = runtime_handle.block_on(crate::group::dispatch_for_message(
            group_for_dispatch,
            trigger_for_dispatch,
            sender_for_dispatch,
        )) {
            eprintln!("warning: group dispatch failed: {e}");
        }
    });

    Ok(StatusCode::CREATED)
}

#[derive(Deserialize)]
struct JoinPayload {
    name: String,
    #[serde(default)]
    github: Option<String>,
}

async fn group_list_joins_handler(
    Path(id): Path<String>,
    Query(query): Query<GroupTokenQuery>,
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
    let token = extract_group_token(&query, &headers);
    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found".to_string()))?;
    if !is_member_token(&group, &token) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "a valid member token is required".to_string(),
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
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "missing name".to_string()));
    }

    let result = crate::group::modify_group_async(&id, move |group| {
        if group.members.is_empty() {
            let token = crate::group::ensure_local_member(group, &name)?;
            return Ok(crate::group::JoinResult {
                id: String::new(),
                status: "approved".to_string(),
                name: name.clone(),
                github: github.clone(),
                member_token: Some(token),
                pre_auth_token: None,
            });
        }
        if crate::group::is_member(group, &name) {
            return Ok(crate::group::JoinResult {
                id: String::new(),
                status: "member".to_string(),
                name: name.clone(),
                github: github.clone(),
                member_token: None,
                pre_auth_token: None,
            });
        }
        let request_id = crate::group::add_join_request(group, &name, github.as_deref())?;
        let pre_auth = group
            .pending_joins
            .iter()
            .find(|r| r.id == request_id)
            .and_then(|r| r.pre_auth_token.clone());
        Ok(crate::group::JoinResult {
            id: request_id,
            status: "pending".to_string(),
            name: name.clone(),
            github,
            member_token: None,
            pre_auth_token: pre_auth,
        })
    })
    .await
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(Json(result))
}

async fn group_approve_join_handler(
    Path((id, request_id)): Path<(String, String)>,
    Query(query): Query<GroupTokenQuery>,
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
    let token = extract_group_token(&query, &headers);
    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found".to_string()))?;
    let _approver = crate::group::validate_member_token(&group, &token).ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            "a valid member token is required to approve".to_string(),
        )
    })?;

    let pre_auth = group
        .pending_joins
        .iter()
        .find(|r| r.id == request_id)
        .and_then(|r| r.pre_auth_token.clone())
        .unwrap_or_default();

    let modify_request_id = request_id.clone();
    let (name, member_token) = crate::group::modify_group_async(&id, move |group| {
        crate::group::approve_join_request(group, &modify_request_id, &pre_auth)
    })
    .await
    .map_err(|e| {
        let msg = e.to_string();
        if msg.contains("not found") {
            (StatusCode::NOT_FOUND, msg)
        } else {
            (StatusCode::BAD_REQUEST, msg)
        }
    })?;

    Ok(Json(crate::group::JoinResult {
        id: request_id,
        status: "approved".to_string(),
        name,
        github: None,
        member_token: Some(member_token),
        pre_auth_token: None,
    }))
}

#[derive(Deserialize)]
struct JoinStatusQuery {
    pre_auth: String,
}

async fn group_join_status_handler(
    Path((id, request_id)): Path<(String, String)>,
    Query(query): Query<JoinStatusQuery>,
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

    let group = crate::group::load_group_async(&id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "group not found".to_string()))?;

    if group.approved_member_tokens.contains_key(&query.pre_auth) {
        let pre_auth = query.pre_auth.clone();
        let member_token = crate::group::modify_group_async(&id, move |g| {
            let token = g.approved_member_tokens.remove(&pre_auth);
            Ok(token)
        })
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to consume approval".to_string(),
            )
        })?;
        if let Some(token) = member_token {
            return Ok(Json(crate::group::JoinResult {
                id: request_id,
                status: "approved".to_string(),
                name: String::new(),
                github: None,
                member_token: Some(token),
                pre_auth_token: None,
            }));
        }
    }

    if group
        .pending_joins
        .iter()
        .any(|r| r.id == request_id && r.pre_auth_token.as_deref() == Some(&query.pre_auth))
    {
        return Ok(Json(crate::group::JoinResult {
            id: request_id,
            status: "pending".to_string(),
            name: String::new(),
            github: None,
            member_token: None,
            pre_auth_token: None,
        }));
    }

    Err((StatusCode::NOT_FOUND, "join request not found".to_string()))
}

#[derive(Deserialize)]
struct RemoteAgentMessagePayload {
    content: String,
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

    let message = crate::group::GroupMessage {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: Utc::now(),
        sender: agent_name.clone(),
        content,
        kind: crate::group::MessageKind::Agent,
    };
    crate::group::add_message_async(&id, &message)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to save message".to_string(),
            )
        })?;

    let group_for_dispatch = crate::group::load_group_async(&id).await.map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to load group".to_string(),
        )
    })?;
    let trigger_for_dispatch = message.clone();
    let runtime_handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = runtime_handle.block_on(crate::group::dispatch_for_message(
            group_for_dispatch,
            trigger_for_dispatch,
            agent_name,
        )) {
            eprintln!("warning: remote agent dispatch failed: {e}");
        }
    });

    Ok(StatusCode::CREATED)
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
    if !crate::group::validate_hosted_agent_token(&id, &name, token) {
        return Err((StatusCode::UNAUTHORIZED, "invalid agent token".to_string()));
    }
    if payload.group_id != id || !payload.agent_name.eq_ignore_ascii_case(&name) {
        return Err((
            StatusCode::BAD_REQUEST,
            "payload group/agent mismatch".to_string(),
        ));
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
    let yolo = payload.yolo;
    let prompt = payload.prompt;
    let content = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(crate::run_single_turn_capture(
            &prompt,
            model,
            yolo,
            Some(1),
            None,
        ))
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(Json(crate::group::RemoteAgentDispatchResponse {
        content: crate::group::truncate_message_content(&content)
            .trim()
            .to_string(),
    }))
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
            if let Some(s) = read_persisted_secret(&path) {
                (s, Some(path), false)
            } else {
                let s = generate_secret();
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
        .route("/acp", get(ws_handler))
        .route("/group", post(admin_create_group_handler))
        .route("/group/{id}", get(group_info_handler))
        .route("/group/{id}/joins", get(group_list_joins_handler))
        .route("/group/{id}/join", post(group_request_join_handler))
        .route(
            "/group/{id}/joins/{request_id}/approve",
            post(group_approve_join_handler),
        )
        .route(
            "/group/{id}/joins/{request_id}/status",
            get(group_join_status_handler),
        )
        .route(
            "/group/{id}/messages",
            get(group_list_messages_handler).post(group_post_message_handler),
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
    let listener = TcpListener::bind(bind_addr).await?;
    let actual_addr = listener.local_addr()?;

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
}
