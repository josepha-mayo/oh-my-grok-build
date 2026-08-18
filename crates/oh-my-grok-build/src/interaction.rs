use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use xai_grok_shell::util::config::{McpServerConfig, McpServerTransportConfig};

pub(crate) const PLAYWRIGHT_SERVER_NAME: &str = "playwright";
pub(crate) const COMPUTER_SERVER_NAME: &str = "computer";
const PLAYWRIGHT_MCP_PACKAGE: &str = "@playwright/mcp@0.0.79";
const SETUP_TIMEOUT: Duration = Duration::from_secs(120);
const SETUP_OUTPUT_LIMIT: usize = 128 * 1024;

pub(crate) fn require_adapter(name: &str, cwd: &Path, command: &str) -> Result<McpServerConfig> {
    let config = xai_grok_shell::util::config::get_mcp_server_config_with_project(name, cwd)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{command} requires the '{name}' MCP adapter; configure it with `omgb mcp add {name} -- <adapter-command> [args...]`"
            )
        })?;
    if !config.enabled {
        bail!("{command} requires the '{name}' MCP adapter, but it is disabled");
    }
    Ok(config)
}

fn resolve_npx() -> Result<std::path::PathBuf> {
    #[cfg(windows)]
    let candidates = ["npx.cmd", "npx"];
    #[cfg(not(windows))]
    let candidates = ["npx", "npx"];

    candidates
        .into_iter()
        .find_map(|candidate| which::which(candidate).ok())
        .context("Node.js 18+ and npx are required to install the Playwright browser adapter")
}

fn official_playwright_args(headless: bool, isolated: bool) -> Vec<String> {
    let mut args = vec!["-y".to_string(), PLAYWRIGHT_MCP_PACKAGE.to_string()];
    if headless {
        args.push("--headless".to_string());
    }
    if isolated {
        args.push("--isolated".to_string());
    }
    args
}

fn is_official_playwright_config(config: &McpServerConfig) -> bool {
    match &config.transport {
        McpServerTransportConfig::Stdio { args, .. } => {
            args.iter().any(|arg| arg == PLAYWRIGHT_MCP_PACKAGE)
        }
        McpServerTransportConfig::StreamableHttp { .. } => false,
    }
}

async fn verify_playwright_package(npx: &Path) -> Result<()> {
    let mut cmd = tokio::process::Command::new(npx);
    cmd.args(["-y", PLAYWRIGHT_MCP_PACKAGE, "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, group) = crate::spawn_with_process_group(cmd)
        .context("failed to start the pinned Playwright MCP package")?;
    let mut stdout = child.stdout.take().context("Playwright stdout not piped")?;
    let mut stderr = child.stderr.take().context("Playwright stderr not piped")?;
    let stdout_task = tokio::spawn(async move {
        let mut capture = crate::BoundedCapture::new(SETUP_OUTPUT_LIMIT + 1);
        tokio::io::copy(&mut stdout, &mut capture).await?;
        Ok::<_, std::io::Error>(capture.into_string())
    });
    let stderr_task = tokio::spawn(async move {
        let mut capture = crate::BoundedCapture::new(SETUP_OUTPUT_LIMIT + 1);
        tokio::io::copy(&mut stderr, &mut capture).await?;
        Ok::<_, std::io::Error>(capture.into_string())
    });
    let status = match tokio::time::timeout(SETUP_TIMEOUT, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            crate::kill_child_and_reap(&mut child, group.as_ref()).await;
            stdout_task.abort();
            stderr_task.abort();
            bail!(
                "Playwright MCP setup timed out after {} seconds",
                SETUP_TIMEOUT.as_secs()
            );
        }
    };
    crate::kill_process_group(group.as_ref());
    let stdout = stdout_task
        .await
        .context("Playwright stdout task failed")??;
    let stderr = stderr_task
        .await
        .context("Playwright stderr task failed")??;
    if stdout.len() > SETUP_OUTPUT_LIMIT || stderr.len() > SETUP_OUTPUT_LIMIT {
        bail!("Playwright MCP setup output exceeded the {SETUP_OUTPUT_LIMIT} byte limit");
    }
    if !status.success() {
        let detail = stderr
            .lines()
            .next()
            .unwrap_or("package verification failed");
        bail!("Playwright MCP package verification failed: {detail}");
    }
    Ok(())
}

pub(crate) async fn setup_playwright(
    cwd: &Path,
    headless: bool,
    isolated: bool,
    force: bool,
) -> Result<()> {
    if let Some(existing) = xai_grok_shell::util::config::get_mcp_server_config_with_project(
        PLAYWRIGHT_SERVER_NAME,
        cwd,
    ) && !force
    {
        if is_official_playwright_config(&existing) && existing.enabled {
            println!("Playwright browser adapter is already configured.");
            return Ok(());
        }
        bail!(
            "an MCP server named '{PLAYWRIGHT_SERVER_NAME}' already exists; rerun with --force-setup to replace the user-scoped definition"
        );
    }

    let npx = resolve_npx()?;
    verify_playwright_package(&npx).await?;
    let config = McpServerConfig {
        transport: McpServerTransportConfig::Stdio {
            command: npx.to_string_lossy().into_owned(),
            args: official_playwright_args(headless, isolated),
            env: None,
            cwd: None,
        },
        enabled: true,
        oauth: None,
        setup: None,
        startup_timeout_sec: Some(60),
        tool_timeout_sec: Some(60),
        tool_timeouts: None,
        expose_image_base64: Some(false),
    };
    xai_grok_shell::util::config::save_mcp_server_config(PLAYWRIGHT_SERVER_NAME, &config)
        .await
        .context("failed to save the Playwright MCP configuration")?;
    let effective = xai_grok_shell::util::config::get_mcp_server_config_with_project(
        PLAYWRIGHT_SERVER_NAME,
        cwd,
    )
    .context("the saved Playwright MCP configuration was not resolvable")?;
    if !is_official_playwright_config(&effective) || !effective.enabled {
        bail!(
            "a project-scoped 'playwright' MCP definition overrides the verified user configuration; remove or rename that project definition before browser use"
        );
    }
    println!(
        "Configured pinned Playwright MCP {PLAYWRIGHT_MCP_PACKAGE} as '{PLAYWRIGHT_SERVER_NAME}'."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_args_are_pinned_and_deterministic() {
        assert_eq!(
            official_playwright_args(false, false),
            vec!["-y", PLAYWRIGHT_MCP_PACKAGE]
        );
        assert_eq!(
            official_playwright_args(true, true),
            vec!["-y", PLAYWRIGHT_MCP_PACKAGE, "--headless", "--isolated"]
        );
        assert!(!PLAYWRIGHT_MCP_PACKAGE.ends_with("@latest"));
    }

    #[test]
    fn official_config_detection_rejects_unrelated_server() {
        let config = McpServerConfig {
            transport: McpServerTransportConfig::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "other-package".into()],
                env: None,
                cwd: None,
            },
            enabled: true,
            oauth: None,
            setup: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            tool_timeouts: None,
            expose_image_base64: None,
        };
        assert!(!is_official_playwright_config(&config));
    }
}
