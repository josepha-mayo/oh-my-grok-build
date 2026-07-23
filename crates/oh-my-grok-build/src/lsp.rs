//! LSP / DAP integration for `omgb`.
//!
//! Phase 2 adds `textDocument/rename` refactoring and DAP attach on top of the
//! JSON-RPC stdio lifecycles.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use url::Url;

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

fn server_for_ext(ext: &str) -> Option<&'static Server> {
    let id = match ext {
        "rs" => "rust-analyzer",
        "ts" | "js" | "tsx" | "jsx" | "mjs" | "cjs" => "typescript-language-server",
        "py" => "basedpyright",
        "go" => "gopls",
        _ => return None,
    };
    LSP_SERVERS.iter().find(|(i, _)| *i == id).map(|(_, s)| s)
}

fn pick_adapter(program: &Path) -> Result<(&'static str, &'static [&'static str])> {
    let ext = program.extension().and_then(|s| s.to_str()).unwrap_or("");
    let ids: &[&str] = match ext {
        "py" => &["debugpy"],
        "go" => &["dlv"],
        "js" | "ts" | "mjs" | "cjs" => &["js-debug-adapter"],
        "dll" | "exe" => &["netcoredbg"],
        "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" | "rs" => &["lldb-dap", "gdb"],
        _ => &[
            "lldb-dap",
            "gdb",
            "netcoredbg",
            "dlv",
            "debugpy",
            "js-debug-adapter",
        ],
    };
    for &id in ids {
        if let Some(&cmd) = adapter_map().get(id)
            && which::which(cmd[0]).is_ok()
        {
            return Ok((id, cmd));
        }
    }
    bail!("no DAP adapter found for {}", program.display())
}

fn lsp_request_payload(id: u64, method: &str, params: serde_json::Value) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
}

fn lsp_notification_payload(method: &str, params: serde_json::Value) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })
}

fn dap_request_payload(seq: u64, command: &str, arguments: serde_json::Value) -> serde_json::Value {
    json!({
        "seq": seq,
        "type": "request",
        "command": command,
        "arguments": arguments,
    })
}

fn encode_message(value: &serde_json::Value) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(value).context("serialize JSON-RPC message")?;
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    Ok(out)
}

struct JsonRpcClient {
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl JsonRpcClient {
    fn new(mut child: Child) -> Result<Self> {
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    async fn lsp_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&lsp_request_payload(id, method, params)).await?;
        self.wait_lsp_response(id).await
    }

    async fn lsp_notify(&mut self, method: &str, params: serde_json::Value) -> Result<()> {
        self.write(&lsp_notification_payload(method, params)).await
    }

    async fn dap_request(
        &mut self,
        command: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let seq = self.next_id;
        self.next_id += 1;
        self.write(&dap_request_payload(seq, command, arguments))
            .await?;
        self.wait_dap_response(seq, command).await
    }

    async fn write(&mut self, msg: &serde_json::Value) -> Result<()> {
        let data = encode_message(msg)?;
        self.stdin
            .write_all(&data)
            .await
            .context("write rpc message")?;
        self.stdin.flush().await.context("flush rpc stream")?;
        Ok(())
    }

    async fn wait_lsp_response(&mut self, id: u64) -> Result<serde_json::Value> {
        loop {
            let msg = self.read_message().await?;
            if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                return Ok(msg);
            }
        }
    }

    async fn wait_dap_response(&mut self, seq: u64, command: &str) -> Result<serde_json::Value> {
        loop {
            let msg = self.read_message().await?;
            if msg.get("type").and_then(|v| v.as_str()) == Some("response")
                && msg.get("command").and_then(|v| v.as_str()) == Some(command)
                && msg.get("request_seq").and_then(|v| v.as_u64()) == Some(seq)
            {
                return Ok(msg);
            }
        }
    }

    /// Keep the adapter process alive and stream its stdout to the terminal
    /// until the process exits. Used by `dap attach` so the debugger is not
    /// killed immediately after the handshake.
    async fn relay(mut self) -> Result<()> {
        let mut child = self.child;
        let copy = tokio::spawn(async move {
            let mut stdout = tokio::io::stdout();
            let _ = tokio::io::copy(&mut self.stdout, &mut stdout).await;
        });
        let status = child.wait().await.context("wait for DAP adapter to exit")?;
        copy.abort();
        if !status.success() {
            bail!(
                "DAP adapter exited with status {}",
                status.code().unwrap_or(-1)
            );
        }
        Ok(())
    }

    async fn read_message(&mut self) -> Result<serde_json::Value> {
        let mut header = String::new();
        let mut len: Option<usize> = None;
        loop {
            header.clear();
            let n = self
                .stdout
                .read_line(&mut header)
                .await
                .context("read header")?;
            if n == 0 {
                bail!("unexpected EOF while reading JSON-RPC header");
            }
            let line = header.trim();
            if line.is_empty() {
                break;
            }
            if let Some(s) = line.strip_prefix("Content-Length:") {
                len = s.trim().parse().ok();
            }
        }
        let len = len.context("missing Content-Length header")?;
        let mut body = vec![0u8; len];
        self.stdout
            .read_exact(&mut body)
            .await
            .context("read body")?;
        serde_json::from_slice(&body).context("parse JSON-RPC body")
    }
}

fn workspace_edit_has_uri(edit: &serde_json::Value, uri: &str) -> bool {
    if let Some(changes) = edit.get("changes").and_then(|c| c.as_object())
        && changes.contains_key(uri)
    {
        return true;
    }
    if let Some(docs) = edit.get("documentChanges").and_then(|d| d.as_array()) {
        return docs.iter().any(|d| {
            d.get("textDocument")
                .and_then(|t| t.get("uri"))
                .and_then(|u| u.as_str())
                == Some(uri)
        });
    }
    false
}

pub async fn lsp_refactor(file_path: &Path, old_name: &str, new_name: &str) -> Result<()> {
    if old_name.is_empty() || new_name.is_empty() {
        bail!("old_name and new_name must not be empty");
    }
    let ext = file_path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let server = server_for_ext(ext)
        .with_context(|| format!("no known LSP server for {}", file_path.display()))?;
    which::which(server.command[0])
        .with_context(|| format!("LSP server {} not found", server.command[0]))?;

    let abs = dunce::canonicalize(file_path)
        .with_context(|| format!("file not found: {}", file_path.display()))?;
    let uri = Url::from_file_path(&abs)
        .map_err(|_| anyhow::anyhow!("invalid file path"))?
        .to_string();
    let root = std::env::current_dir()?;
    let root_uri = Url::from_file_path(&root)
        .map_err(|_| anyhow::anyhow!("invalid root path"))?
        .to_string();

    let mut cmd = tokio::process::Command::new(server.command[0]);
    cmd.args(&server.command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .current_dir(&root);
    let child = cmd
        .spawn()
        .with_context(|| format!("spawn LSP server {}", server.command[0]))?;
    let mut client = JsonRpcClient::new(child)?;

    let _init = client
        .lsp_request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {},
                "workspaceFolders": [{"uri": root_uri, "name": "root"}],
            }),
        )
        .await?;
    client.lsp_notify("initialized", json!({})).await?;

    let content = tokio::fs::read_to_string(&abs).await?;
    let language_id = server.languages.first().copied().unwrap_or("");
    client
        .lsp_notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": content,
                }
            }),
        )
        .await?;

    let (line, character) = find_position(&content, old_name)
        .with_context(|| format!("symbol {old_name} not found in {}", file_path.display()))?;

    let rename_resp = client
        .lsp_request(
            "textDocument/rename",
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
                "newName": new_name,
            }),
        )
        .await?;

    let result = rename_resp.get("result");
    let result_has_edits = result.is_some_and(|r| workspace_edit_has_uri(r, &uri));
    let edit = if result_has_edits {
        result.cloned().unwrap()
    } else {
        let end_char = character + old_name.chars().count() as u64;
        json!({
            "changes": {
                (uri): [{
                    "range": {
                        "start": {"line": line, "character": character},
                        "end": {"line": line, "character": end_char}
                    },
                    "newText": new_name
                }]
            }
        })
    };

    apply_workspace_edit(&edit).await?;
    Ok(())
}

pub async fn dap_attach(program: &Path, pid: u32, extra_args: &[String]) -> Result<()> {
    let (id, cmd) = pick_adapter(program)?;
    which::which(cmd[0]).with_context(|| format!("DAP adapter {id} not found"))?;

    let mut command = tokio::process::Command::new(cmd[0]);
    command
        .args(&cmd[1..])
        .args(extra_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .with_context(|| format!("spawn DAP adapter {id}"))?;
    let mut client = JsonRpcClient::new(child)?;

    let program_abs = dunce::canonicalize(program).unwrap_or_else(|_| program.to_path_buf());
    let program_str = program_abs.to_string_lossy().to_string();

    let _ = client
        .dap_request(
            "initialize",
            json!({
                "clientID": "omgb",
                "clientName": "omgb",
                "adapterID": id,
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "supportsVariableType": true,
                "supportsRunInTerminalRequest": false,
            }),
        )
        .await?;

    let mut attach_args = json!({
        "program": program_str,
        "pid": pid,
        "processId": pid,
        "request": "attach",
        "type": id,
    });
    for arg in extra_args {
        if let Some((k, v)) = arg.split_once('=')
            && let Some(obj) = attach_args.as_object_mut()
        {
            obj.insert(k.to_string(), json!(v));
        }
    }

    let _ = client.dap_request("attach", attach_args).await?;
    client.relay().await
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
        LspCommand::Refactor {
            file,
            old_name,
            new_name,
        } => lsp_refactor(&file, &old_name, &new_name).await?,
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
        DapCommand::Attach {
            program,
            pid,
            extra,
        } => dap_attach(&program, pid, &extra).await?,
    }
    Ok(())
}

async fn relay_stdio(mut child: tokio::process::Child) -> Result<()> {
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut child_stdin = child.stdin.take().context("no stdin")?;
    let mut child_stdout = child.stdout.take().context("no stdout")?;
    let mut child_stderr = child.stderr.take().context("no stderr")?;

    let stdin_to_child = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stdin, &mut child_stdin).await;
    });
    let stdout_to_term = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut child_stdout, &mut stdout).await;
    });
    let stderr_to_term = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut child_stderr, &mut stderr).await;
    });

    let status = child.wait().await.context("wait for server")?;
    stdin_to_child.abort();
    stdout_to_term.abort();
    stderr_to_term.abort();

    if !status.success() {
        bail!("server exited with {}", status.code().unwrap_or(-1));
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
        .kill_on_drop(true)
        .current_dir(&args.cwd);
    let child = cmd.spawn().with_context(|| "spawn LSP server")?;
    println!(
        "started {} for {} (pid {})",
        args.server,
        languages.join(", "),
        child.id().unwrap_or(0)
    );
    relay_stdio(child).await
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
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| "spawn debug adapter")?;
    println!(
        "started DAP adapter {adapter} (pid {})",
        child.id().unwrap_or(0)
    );
    relay_stdio(child).await
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn find_position(content: &str, old_name: &str) -> Result<(u64, u64)> {
    for (line_num, line) in content.lines().enumerate() {
        let mut start = 0;
        while let Some(pos) = line[start..].find(old_name) {
            let pos = start + pos;
            let end = pos + old_name.len();
            let prev = line[..pos].chars().last();
            let next = line[end..].chars().next();
            if !prev.is_some_and(is_word_char) && !next.is_some_and(is_word_char) {
                let char_pos = line[..pos].chars().count();
                return Ok((line_num as u64, char_pos as u64));
            }
            start = end;
        }
    }
    bail!("symbol not found")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Position {
    line: u64,
    character: u64,
}

fn parse_position(v: &serde_json::Value) -> Result<Position> {
    let line = v
        .get("line")
        .and_then(|l| l.as_u64())
        .context("missing line")?;
    let character = v
        .get("character")
        .and_then(|c| c.as_u64())
        .context("missing character")?;
    Ok(Position { line, character })
}

fn position_to_byte(text: &str, pos: Position) -> Result<usize> {
    let mut line_idx = 0usize;
    let mut char_count = 0usize;
    for (i, c) in text.chars().enumerate() {
        if line_idx == pos.line as usize {
            if char_count == pos.character as usize {
                return Ok(i);
            }
            char_count += 1;
        }
        if c == '\n' {
            line_idx += 1;
            if line_idx > pos.line as usize {
                break;
            }
            char_count = 0;
        }
    }
    if line_idx == pos.line as usize && char_count == pos.character as usize {
        return Ok(text.len());
    }
    bail!("position out of range")
}

fn url_to_path(uri: &str) -> Result<std::path::PathBuf> {
    let url = Url::parse(uri).with_context(|| format!("invalid URI: {uri}"))?;
    url.to_file_path()
        .map_err(|_| anyhow::anyhow!("URI is not a file path: {uri}"))
}

async fn apply_text_edits_to_path(uri: &str, edits_value: &serde_json::Value) -> Result<()> {
    let path = url_to_path(uri)?;
    let mut edits = edits_value
        .as_array()
        .context("edits must be an array")?
        .iter()
        .map(|e| {
            let range = e.get("range").context("edit missing range")?;
            let start = parse_position(range.get("start").context("start")?)?;
            let end = parse_position(range.get("end").context("end")?)?;
            let new_text = e
                .get("newText")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok((start, end, new_text))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut text = tokio::fs::read_to_string(&path).await?;
    edits.sort_by(|a, b| b.0.cmp(&a.0));
    for (start, end, new_text) in edits {
        let start_idx = position_to_byte(&text, start)?;
        let end_idx = position_to_byte(&text, end)?;
        text.replace_range(start_idx..end_idx, &new_text);
    }
    tokio::fs::write(&path, text.as_bytes()).await?;
    Ok(())
}

async fn apply_workspace_edit(edit: &serde_json::Value) -> Result<()> {
    if let Some(changes) = edit.get("changes") {
        let changes = changes
            .as_object()
            .context("workspace changes must be an object")?;
        for (uri, edits_value) in changes {
            apply_text_edits_to_path(uri, edits_value).await?;
        }
        return Ok(());
    }

    if let Some(document_changes) = edit.get("documentChanges") {
        let document_changes = document_changes
            .as_array()
            .context("documentChanges must be an array")?;
        for change in document_changes {
            if let Some(text_document) = change.get("textDocument") {
                let uri = text_document
                    .get("uri")
                    .and_then(|u| u.as_str())
                    .context("missing textDocument uri")?;
                if let Some(edits) = change.get("edits") {
                    apply_text_edits_to_path(uri, edits).await?;
                }
            } else if change.get("kind").is_some() {
                bail!("workspace file operations are not supported in refactor");
            }
        }
        return Ok(());
    }

    bail!("workspace edit has neither changes nor documentChanges")
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

    #[test]
    fn lsp_initialize_payload_well_formed() {
        let msg = lsp_request_payload(
            1,
            "initialize",
            json!({"processId": 123, "rootUri": "file:///tmp", "capabilities": {}}),
        );
        let bytes = encode_message(&msg).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let header_end = text.find("\r\n\r\n").unwrap();
        let body = &text[header_end + 4..];
        let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["id"], 1);
        assert_eq!(parsed["method"], "initialize");
        assert!(parsed["params"].is_object());
    }

    #[test]
    fn lsp_rename_payload_well_formed() {
        let msg = lsp_request_payload(
            2,
            "textDocument/rename",
            json!({
                "textDocument": {"uri": "file:///tmp/main.rs"},
                "position": {"line": 0, "character": 4},
                "newName": "bar",
            }),
        );
        let bytes = encode_message(&msg).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let header_end = text.find("\r\n\r\n").unwrap();
        let body = &text[header_end + 4..];
        let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["method"], "textDocument/rename");
        assert_eq!(parsed["params"]["newName"], "bar");
    }

    #[test]
    fn lsp_did_open_payload_well_formed() {
        let msg = lsp_notification_payload(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///tmp/main.rs",
                    "languageId": "rust",
                    "version": 1,
                    "text": "fn foo() {}",
                }
            }),
        );
        let bytes = encode_message(&msg).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let header_end = text.find("\r\n\r\n").unwrap();
        let body = &text[header_end + 4..];
        let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["method"], "textDocument/didOpen");
        assert_eq!(parsed["params"]["textDocument"]["languageId"], "rust");
    }

    #[test]
    fn dap_initialize_payload_well_formed() {
        let msg = dap_request_payload(
            1,
            "initialize",
            json!({"clientID": "omgb", "adapterID": "debugpy"}),
        );
        let bytes = encode_message(&msg).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let header_end = text.find("\r\n\r\n").unwrap();
        let body = &text[header_end + 4..];
        let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["seq"], 1);
        assert_eq!(parsed["type"], "request");
        assert_eq!(parsed["command"], "initialize");
    }

    #[test]
    fn dap_attach_payload_well_formed() {
        let args = json!({
            "program": "main.py",
            "pid": 1234,
            "processId": 1234,
            "request": "attach",
            "type": "debugpy",
        });
        let msg = dap_request_payload(2, "attach", args);
        let bytes = encode_message(&msg).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let header_end = text.find("\r\n\r\n").unwrap();
        let body = &text[header_end + 4..];
        let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["command"], "attach");
        assert_eq!(parsed["arguments"]["pid"], 1234);
        assert_eq!(parsed["arguments"]["request"], "attach");
    }

    #[test]
    fn find_position_finds_symbol() {
        let text = "fn foo() {}\nlet x = foo;\nlet foobar = 1;";
        let (line, character) = find_position(text, "foo").unwrap();
        assert_eq!(line, 0);
        assert_eq!(character, 3);
    }

    #[test]
    fn find_position_respects_word_boundaries() {
        assert!(find_position("let foobar = 1;", "foo").is_err());
    }
}
