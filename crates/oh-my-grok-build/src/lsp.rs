//! LSP / DAP integration stubs for `omgb`.
//!
//! Provides commands to list known language servers and debug adapters and to
//! start them.  Full semantic refactoring and debugger attach are left as Phase
//! 3 work built on top of these JSON-RPC stdio lifecycles.

use std::collections::HashMap;
use std::process::Stdio;

use anyhow::{Context, Result};

use crate::args::{DapCommand, LspCommand, LspStartArgs};

#[derive(Clone)]
struct Server {
    languages: &'static [&'static str],
    command: &'static [&'static str],
}

static LSP_SERVERS: &[(&str, Server)] = &[
    (
        "rust-analyzer",
        Server {
            languages: &["rust"],
            command: &["rust-analyzer"],
        },
    ),
    (
        "typescript-language-server",
        Server {
            languages: &["typescript", "javascript"],
            command: &["typescript-language-server", "--stdio"],
        },
    ),
    (
        "basedpyright",
        Server {
            languages: &["python"],
            command: &["basedpyright-langserver", "--stdio"],
        },
    ),
    (
        "pylsp",
        Server {
            languages: &["python"],
            command: &["pylsp"],
        },
    ),
    (
        "gopls",
        Server {
            languages: &["go"],
            command: &["gopls"],
        },
    ),
];

static DAP_ADAPTERS: &[(&str, &[&str])] = &[
    ("gdb", &["gdb", "--interpreter=mi"]),
    ("lldb-dap", &["lldb-dap"]),
    ("debugpy", &["python", "-m", "debugpy.adapter"]),
    ("dlv", &["dlv", "dap"]),
    ("js-debug-adapter", &["js-debug-adapter"]),
    ("netcoredbg", &["netcoredbg", "--interpreter=vscode"]),
];

fn server_map() -> HashMap<&'static str, &'static Server> {
    LSP_SERVERS.iter().map(|(id, s)| (*id, s)).collect()
}

fn adapter_map() -> HashMap<&'static str, &'static [&'static str]> {
    DAP_ADAPTERS.iter().map(|(id, cmd)| (*id, *cmd)).collect()
}

pub async fn run_lsp(cmd: LspCommand) -> Result<()> {
    match cmd {
        LspCommand::List => {
            for (id, s) in LSP_SERVERS {
                println!(
                    "{} ({}): {}",
                    id,
                    s.languages.join(", "),
                    s.command.join(" ")
                );
            }
        }
        LspCommand::Start(args) => start_lsp(&args).await?,
    }
    Ok(())
}

pub async fn run_dap(cmd: DapCommand) -> Result<()> {
    match cmd {
        DapCommand::List => {
            for (id, cmd) in DAP_ADAPTERS {
                println!("{}: {}", id, cmd.join(" "));
            }
        }
        DapCommand::Start(args) => start_adapter(&args.adapter, args.extra.as_slice()).await?,
    }
    Ok(())
}

async fn start_lsp(args: &LspStartArgs) -> Result<()> {
    let map = server_map();
    let server = map
        .get(args.server.as_str())
        .with_context(|| format!("unknown LSP server: {}", args.server))?;
    let languages = if args.languages.is_empty() {
        server
            .languages
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    } else {
        args.languages.clone()
    };
    let mut cmd = tokio::process::Command::new(server.command[0]);
    cmd.args(&server.command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(&args.cwd);
    let child = cmd.spawn().with_context(|| "spawn LSP server")?;
    let pid = child.id().unwrap_or(0);
    println!(
        "started {} for {} (pid {})",
        args.server,
        languages.join(", "),
        pid
    );
    // Leave the server running; the user can attach later.  On Windows this
    // drops our handles but the child keeps running because we do not wait.
    let _ = child;
    Ok(())
}

async fn start_adapter(adapter: &str, extra: &[String]) -> Result<()> {
    let map = adapter_map();
    let cmd = map
        .get(adapter)
        .with_context(|| format!("unknown DAP adapter: {adapter}"))?;
    let child = tokio::process::Command::new(cmd[0])
        .args(&cmd[1..])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .with_context(|| "spawn debug adapter")?;
    let pid = child.id().unwrap_or(0);
    println!("started DAP adapter {adapter} (pid {pid})");
    let _ = child;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_server_lookup() {
        let m = server_map();
        assert!(m.contains_key("rust-analyzer"));
        assert!(m.contains_key("gopls"));
    }

    #[test]
    fn dap_adapter_lookup() {
        let m = adapter_map();
        assert!(m.contains_key("debugpy"));
        assert!(m.contains_key("gdb"));
    }
}
