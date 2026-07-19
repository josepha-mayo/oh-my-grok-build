//! ACP/WebSocket relay helpers for `omgb serve` and `omgb connect`.

use std::net::SocketAddr;

use anyhow::Result;

use crate::args::{ConnectArgs, ServeArgs};

fn build_agent_config(model: Option<String>) -> Result<xai_grok_shell::agent::config::Config> {
    let raw = xai_grok_shell::config::load_effective_config_disk_only()
        .map_err(|e| anyhow::anyhow!("failed to load config: {e}"))?;
    let mut cfg = xai_grok_shell::agent::config::Config::new_from_toml_cfg(&raw)
        .map_err(|e| anyhow::anyhow!("failed to create agent config: {e}"))?;
    cfg.default_model_override = model;
    Ok(cfg)
}

pub async fn serve(args: &ServeArgs) -> Result<()> {
    let mut agent_config = build_agent_config(args.model.clone())?;
    agent_config.default_yolo_mode = args.yolo;

    let secret = args.secret.clone().unwrap_or_else(|| {
        uuid::Uuid::new_v4()
            .to_string()
            .replace('-', "")
            .chars()
            .take(12)
            .collect()
    });
    let bind_addr = args.bind;

    println!("oh-my-grok-build serve");
    println!("  bind: {bind_addr}");
    println!("  secret: {secret}");

    let server_config = xai_grok_shell::agent::ServerConfig { bind_addr, secret };
    xai_grok_shell::agent::run_agent_server(server_config, agent_config).await?;
    Ok(())
}

pub async fn connect(args: &ConnectArgs) -> Result<()> {
    let mut agent_config = build_agent_config(args.model.clone())?;
    agent_config.grok_com_config.grok_ws_url = Some(args.url.clone());
    if let Some(secret) = &args.secret {
        agent_config.grok_com_config.grok_ws_origin =
            Some(format!("wss://{secret}@{}?server-key={secret}", args.url));
    }

    println!("oh-my-grok-build connect to {}", args.url);
    xai_grok_shell::agent::app::run_headless(&agent_config, false, None).await?;
    Ok(())
}
