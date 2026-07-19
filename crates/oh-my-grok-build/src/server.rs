//! ACP/WebSocket relay helpers for `omgb serve` and `omgb connect`.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use anyhow::Result;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::args::{ConnectArgs, ServeArgs};

fn build_agent_config(model: Option<String>) -> Result<xai_grok_shell::agent::config::Config> {
    let raw = xai_grok_shell::config::load_effective_config_disk_only()
        .map_err(|e| anyhow::anyhow!("failed to load config: {e}"))?;
    let mut cfg = xai_grok_shell::agent::config::Config::new_from_toml_cfg(&raw)
        .map_err(|e| anyhow::anyhow!("failed to create agent config: {e}"))?;
    cfg.default_model_override = model;
    Ok(cfg)
}

fn omg_dir() -> PathBuf {
    crate::providers::omg_dir()
}

fn generate_secret() -> String {
    uuid::Uuid::new_v4()
        .to_string()
        .replace('-', "")
        .chars()
        .take(12)
        .collect()
}

fn pairing_url(bind_addr: SocketAddr, secret: &str) -> String {
    let host = match bind_addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => "127.0.0.1".to_string(),
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) if ip.is_unspecified() => "[::1]".to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    format!("ws://{host}:{}?server-key={secret}", bind_addr.port())
}

fn print_pairing_info(bind_addr: SocketAddr, secret: &str) {
    let url = pairing_url(bind_addr, secret);
    println!("  pairing url: {url}");
    match qrcode::QrCode::new(url.as_bytes()) {
        Ok(code) => {
            let qr = code.render().dark_color('#').light_color(' ').build();
            println!("  pairing QR:");
            for line in qr.lines() {
                println!("    {line}");
            }
        }
        Err(_) => {}
    }
}

pub async fn serve(args: &ServeArgs) -> Result<()> {
    let mut agent_config = build_agent_config(args.model.clone())?;
    agent_config.default_yolo_mode = args.yolo;

    let (secret, secret_path, provided) = match &args.secret {
        Some(s) => (s.clone(), None, true),
        None => {
            let dir = omg_dir();
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("serve.secret");
            let s = generate_secret();
            std::fs::write(&path, &s)?;
            #[cfg(unix)]
            {
                use std::fs::Permissions;
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, Permissions::from_mode(0o600))?;
            }
            (s, Some(path), false)
        }
    };

    let bind_addr = args.bind;

    println!("oh-my-grok-build serve");
    println!("  bind: {bind_addr}");
    if let Some(path) = &secret_path {
        println!("  secret file: {}", path.display());
    } else if provided {
        println!("  secret: <provided>");
    }
    print_pairing_info(bind_addr, &secret);

    let server_config = xai_grok_shell::agent::ServerConfig { bind_addr, secret };
    xai_grok_shell::agent::run_agent_server(server_config, agent_config).await?;
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

    if let Some(secret) = &args.secret {
        url.query_pairs_mut().append_pair("server-key", secret);
    }

    let (ws_stream, _) = tokio_tungstenite::connect_async(url.as_str()).await?;
    println!("Connected to {}", args.url);

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
                        if write.send(Message::Text(text.trim_end().to_string())).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => anyhow::bail!("stdin read error: {e}"),
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(t))) => println!("{}", t),
                    Some(Ok(Message::Binary(b))) => println!("{}", String::from_utf8_lossy(&b)),
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(e)) => anyhow::bail!("websocket error: {e}"),
                    _ => {}
                }
            }
        }
    }

    Ok(())
}
