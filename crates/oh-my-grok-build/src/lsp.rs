//! LSP / DAP integration for `omgb`.
//!
//! Phase 2 adds `textDocument/rename` refactoring and DAP attach on top of the
//! JSON-RPC stdio lifecycles.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use url::Url;

use crate::args::{DapCommand, LspCommand, LspStartArgs};

const MAX_JSONRPC_MESSAGE_SIZE: usize = 8 * 1024 * 1024;
const LSP_TRANSACTION_DIR: &str = ".omgb-lsp-transaction";
const MAX_TRANSACTION_ENTRIES: usize = 1024;
const MAX_TRANSACTION_FILE_SIZE: usize = 8 * 1024 * 1024;

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
        "dll" | "exe" => &["lldb-dap", "gdb", "netcoredbg"],
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
        match tokio::time::timeout(Duration::from_secs(30), self.wait_lsp_response(id)).await {
            Ok(Ok(msg)) => Ok(msg),
            Ok(Err(e)) => Err(e),
            Err(_) => bail!("LSP request '{method}' timed out after 30s"),
        }
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
        match tokio::time::timeout(
            Duration::from_secs(30),
            self.wait_dap_response(seq, command),
        )
        .await
        {
            Ok(Ok(msg)) => Ok(msg),
            Ok(Err(e)) => Err(e),
            Err(_) => bail!("DAP request '{command}' timed out after 30s"),
        }
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
        if len > MAX_JSONRPC_MESSAGE_SIZE {
            bail!("JSON-RPC message size {len} exceeds {MAX_JSONRPC_MESSAGE_SIZE}");
        }
        let mut body = vec![0u8; len];
        self.stdout
            .read_exact(&mut body)
            .await
            .context("read body")?;
        serde_json::from_slice(&body).context("parse JSON-RPC body")
    }
}

fn rename_workspace_edit(response: &serde_json::Value) -> Result<&serde_json::Value> {
    if let Some(error) = response.get("error") {
        let code = error.get("code").and_then(|value| value.as_i64());
        let message = error
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("language server rejected the rename");
        bail!(
            "language server rename failed{}: {message}",
            code.map_or_else(String::new, |code| format!(" ({code})"))
        );
    }
    let result = response
        .get("result")
        .context("language server rename response is missing result")?;
    if result.is_null() {
        bail!("language server produced no semantic rename edit");
    }
    if !result.is_object() {
        bail!("language server returned an invalid semantic rename edit");
    }
    Ok(result)
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
    let root = dunce::canonicalize(std::env::current_dir()?)
        .with_context(|| "failed to canonicalize workspace root")?;
    let recovery_root = root.clone();
    tokio::task::spawn_blocking(move || recover_workspace_transaction(&recovery_root))
        .await
        .context("workspace edit recovery task panicked")??;
    let root_uri = Url::from_file_path(&root)
        .map_err(|_| anyhow::anyhow!("invalid root path"))?
        .to_string();

    let server_path = which::which(server.command[0])
        .with_context(|| format!("LSP server {} not found", server.command[0]))?;
    let mut cmd = tokio::process::Command::new(server_path);
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

    let edit = rename_workspace_edit(&rename_resp)?;
    apply_workspace_edit(edit, &root).await?;
    Ok(())
}

pub async fn dap_attach(program: &Path, pid: u32, extra_args: &[String]) -> Result<()> {
    validate_dap_target(program, pid)?;
    let (id, cmd) = pick_adapter(program)?;
    let adapter = which::which(cmd[0]).with_context(|| format!("DAP adapter {id} not found"))?;

    let mut command = tokio::process::Command::new(adapter);
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
            && !k.is_empty()
            && !v.is_empty()
            && k.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
            && let Some(obj) = attach_args.as_object_mut()
        {
            obj.insert(k.to_string(), json!(v));
        }
    }

    let _ = client.dap_request("attach", attach_args).await?;
    client.relay().await
}

fn validate_dap_target(program: &Path, pid: u32) -> Result<()> {
    if pid == 0 {
        bail!("cannot attach to PID 0");
    }
    if pid == std::process::id() {
        bail!("cannot attach to the current process");
    }
    if !crate::process_alive(pid) {
        bail!("process {pid} is not alive");
    }
    if !is_process_owned_by_current_user(pid)? {
        bail!("process {pid} is not owned by the current user");
    }
    let expected = resolve_program_path(program)?;
    let actual = process_image_path(pid)?;
    if !same_executable(&expected, &actual) {
        bail!(
            "process {pid} image ({}) does not match program {}",
            actual.display(),
            expected.display()
        );
    }
    Ok(())
}

fn resolve_program_path(program: &Path) -> Result<PathBuf> {
    let candidate = if program.is_absolute() {
        program.to_path_buf()
    } else {
        which::which(program)
            .or_else(|_| dunce::canonicalize(program))
            .with_context(|| format!("program not found: {}", program.display()))?
    };
    dunce::canonicalize(&candidate)
        .with_context(|| format!("program path is not resolvable: {}", candidate.display()))
}

pub(crate) fn same_executable(a: &Path, b: &Path) -> bool {
    match (dunce::canonicalize(a), dunce::canonicalize(b)) {
        (Ok(a), Ok(b)) => {
            if cfg!(windows) {
                a.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
            } else {
                a == b
            }
        }
        _ => false,
    }
}

#[cfg(unix)]
fn is_process_owned_by_current_user(pid: u32) -> Result<bool> {
    let me = unsafe { libc::getuid() } as u32;
    let status_path = format!("/proc/{pid}/status");
    if let Ok(status) = std::fs::read_to_string(&status_path) {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                let uid = rest
                    .split_whitespace()
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("malformed Uid line for {pid}"))?
                    .parse::<u32>()
                    .with_context(|| format!("parse Uid for {pid}"))?;
                return Ok(uid == me);
            }
        }
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "uid=", "-p", &pid.to_string()])
        .output()
        .context("failed to run ps for process ownership")?;
    if !out.status.success() {
        bail!("ps failed to query process ownership");
    }
    let uid = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u32>()
        .context("ps output is not a uid")?;
    Ok(uid == me)
}

#[cfg(target_os = "linux")]
pub(crate) fn process_image_path(pid: u32) -> Result<PathBuf> {
    let exe = format!("/proc/{pid}/exe");
    if let Ok(path) = std::fs::read_link(&exe) {
        return Ok(path);
    }
    let cmdline = std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
        .with_context(|| format!("cannot read /proc/{pid}/cmdline"))?;
    let first = cmdline
        .split_terminator('\0')
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty cmdline for {pid}"))?;
    Ok(PathBuf::from(first))
}

#[cfg(target_os = "macos")]
pub(crate) fn process_image_path(pid: u32) -> Result<PathBuf> {
    let mut buffer = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let written =
        unsafe { libc::proc_pidpath(pid as i32, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
    if written <= 0 {
        bail!("proc_pidpath({pid}) failed");
    }
    let end = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(written as usize);
    Ok(PathBuf::from(std::ffi::OsStr::new(
        std::str::from_utf8(&buffer[..end]).context("process path is not UTF-8")?,
    )))
}

#[cfg(windows)]
fn is_process_owned_by_current_user(pid: u32) -> Result<bool> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TokenUser};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_INFORMATION,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let mut current_token = windows::Win32::Foundation::HANDLE::default();
        let mut handle = windows::Win32::Foundation::HANDLE::default();
        let mut target_token = windows::Win32::Foundation::HANDLE::default();

        let result = (|| {
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut current_token)
                .map_err(|e| anyhow::anyhow!("OpenProcessToken(current): {e}"))?;
            let mut current_len = 0u32;
            let _ = GetTokenInformation(current_token, TokenUser, None, 0, &mut current_len);
            let mut current_buf = vec![0u8; current_len as usize];
            GetTokenInformation(
                current_token,
                TokenUser,
                Some(current_buf.as_mut_ptr() as *mut _),
                current_len,
                &mut current_len,
            )
            .map_err(|e| anyhow::anyhow!("GetTokenInformation(current): {e}"))?;
            let current_sid = crate::win_sid::sid_ptr(&current_buf)?;

            handle = OpenProcess(
                PROCESS_QUERY_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION,
                false,
                pid,
            )
            .map_err(|e| anyhow::anyhow!("OpenProcess({pid}): {e}"))?;
            OpenProcessToken(handle, TOKEN_QUERY, &mut target_token)
                .map_err(|e| anyhow::anyhow!("OpenProcessToken({pid}): {e}"))?;
            let mut target_len = 0u32;
            let _ = GetTokenInformation(target_token, TokenUser, None, 0, &mut target_len);
            let mut target_buf = vec![0u8; target_len as usize];
            GetTokenInformation(
                target_token,
                TokenUser,
                Some(target_buf.as_mut_ptr() as *mut _),
                target_len,
                &mut target_len,
            )
            .map_err(|e| anyhow::anyhow!("GetTokenInformation({pid}): {e}"))?;
            let target_sid = crate::win_sid::sid_ptr(&target_buf)?;

            Ok(crate::win_sid::sid_bytes(&current_buf, current_sid)?
                == crate::win_sid::sid_bytes(&target_buf, target_sid)?)
        })();

        for token in [target_token, handle, current_token] {
            if !token.is_invalid() {
                let _ = CloseHandle(token);
            }
        }
        result
    }
}

#[cfg(windows)]
pub(crate) fn process_image_path(pid: u32) -> Result<PathBuf> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|e| anyhow::anyhow!("OpenProcess({pid}): {e}"))?;
        let mut buf: Vec<u16> = vec![0; 1024];
        let mut size: u32 = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        result.map_err(|e| anyhow::anyhow!("QueryFullProcessImageNameW({pid}): {e}"))?;
        Ok(PathBuf::from(String::from_utf16_lossy(
            &buf[..size as usize],
        )))
    }
}

#[cfg(windows)]
pub(crate) fn process_start_identity(pid: u32) -> Result<u64> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
            .map_err(|error| anyhow::anyhow!("OpenProcess({pid}): {error}"))?;
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let result = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
        let _ = CloseHandle(handle);
        result.map_err(|error| anyhow::anyhow!("GetProcessTimes({pid}): {error}"))?;
        Ok(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
    }
}

#[cfg(windows)]
pub(crate) fn process_parent_pid(pid: u32) -> Result<Option<u32>> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|error| anyhow::anyhow!("CreateToolhelp32Snapshot: {error}"))?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let result = (|| -> Result<Option<u32>> {
            Process32FirstW(snapshot, &mut entry)
                .map_err(|error| anyhow::anyhow!("Process32FirstW: {error}"))?;
            loop {
                if entry.th32ProcessID == pid {
                    return Ok(
                        (entry.th32ParentProcessID != 0).then_some(entry.th32ParentProcessID)
                    );
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    return Ok(None);
                }
            }
        })();
        let _ = CloseHandle(snapshot);
        result
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn process_start_identity(pid: u32) -> Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat
        .rsplit_once(')')
        .map(|(_, fields)| fields)
        .ok_or_else(|| anyhow::anyhow!("invalid /proc process stat"))?;
    fields
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| anyhow::anyhow!("process start time is missing"))?
        .parse()
        .context("parse process start time")
}

#[cfg(target_os = "macos")]
pub(crate) fn process_start_identity(pid: u32) -> Result<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let written = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        bail!("proc_pidinfo({pid}) failed");
    }
    let info = unsafe { info.assume_init() };
    Ok(info
        .pbi_start_tvsec
        .saturating_mul(1_000_000)
        .saturating_add(info.pbi_start_tvusec))
}

#[cfg(target_os = "linux")]
pub(crate) fn process_parent_pid(pid: u32) -> Result<Option<u32>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields = stat
        .rsplit_once(')')
        .map(|(_, fields)| fields)
        .ok_or_else(|| anyhow::anyhow!("invalid /proc process stat"))?;
    let parent: u32 = fields
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("process parent pid is missing"))?
        .parse()
        .context("parse process parent pid")?;
    Ok((parent != 0).then_some(parent))
}

#[cfg(target_os = "macos")]
pub(crate) fn process_parent_pid(pid: u32) -> Result<Option<u32>> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let written = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        bail!("proc_pidinfo({pid}) failed");
    }
    let parent = unsafe { info.assume_init() }.pbi_ppid;
    Ok((parent != 0).then_some(parent))
}

#[cfg(not(any(unix, windows)))]
fn is_process_owned_by_current_user(_pid: u32) -> Result<bool> {
    bail!("DAP attach is not supported on this platform")
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn process_image_path(_pid: u32) -> Result<PathBuf> {
    bail!("DAP attach is not supported on this platform")
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn process_start_identity(_pid: u32) -> Result<u64> {
    bail!("process start identity is not supported on this platform")
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) fn process_parent_pid(_pid: u32) -> Result<Option<u32>> {
    bail!("process ancestry is not supported on this platform")
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
    let server_path = which::which(server.command[0])
        .with_context(|| format!("LSP server {} not found", server.command[0]))?;
    let mut cmd = tokio::process::Command::new(server_path);
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
    let adapter_path =
        which::which(cmd[0]).with_context(|| format!("DAP adapter {adapter} not found"))?;
    let child = tokio::process::Command::new(adapter_path)
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

fn utf16_len(s: &str) -> u64 {
    s.chars().map(|c| c.len_utf16() as u64).sum()
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
                return Ok((line_num as u64, utf16_len(&line[..pos])));
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
    let mut utf16_count = 0u64;
    for (i, c) in text.char_indices() {
        if line_idx == pos.line as usize {
            if utf16_count == pos.character {
                return Ok(i);
            }
            utf16_count += c.len_utf16() as u64;
        }
        if c == '\n' {
            line_idx += 1;
            if line_idx > pos.line as usize {
                break;
            }
            utf16_count = 0;
        }
    }
    if line_idx == pos.line as usize && utf16_count == pos.character {
        return Ok(text.len());
    }
    bail!("position out of range")
}

fn url_to_path(uri: &str, root: &Path) -> Result<std::path::PathBuf> {
    let url = Url::parse(uri).with_context(|| format!("invalid URI: {uri}"))?;
    let path = url
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("URI is not a file path: {uri}"))?;
    let path = dunce::canonicalize(&path)
        .with_context(|| format!("workspace edit URI does not exist: {uri}"))?;
    let root = dunce::canonicalize(root)
        .with_context(|| format!("workspace root does not exist: {}", root.display()))?;
    if !path.starts_with(&root) {
        bail!(
            "workspace edit URI {} is outside workspace {}",
            path.display(),
            root.display()
        );
    }
    Ok(path)
}

fn apply_text_edits(text: &str, edits_value: &serde_json::Value) -> Result<String> {
    let mut edits = edits_value
        .as_array()
        .context("edits must be an array")?
        .iter()
        .map(|e| {
            let range = e.get("range").context("edit missing range")?;
            let start = parse_position(range.get("start").context("start")?)?;
            let end = parse_position(range.get("end").context("end")?)?;
            if end < start {
                bail!("edit range end precedes its start");
            }
            let new_text = e
                .get("newText")
                .and_then(|v| v.as_str())
                .context("edit missing newText")?
                .to_string();
            Ok((start, end, new_text))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut text = text.to_string();
    edits.sort_by(|a, b| b.0.cmp(&a.0));
    for pair in edits.windows(2) {
        if pair[1].1 > pair[0].0 {
            bail!("workspace edit contains overlapping ranges");
        }
    }
    for (start, end, new_text) in edits {
        let start_idx = position_to_byte(&text, start)?;
        let end_idx = position_to_byte(&text, end)?;
        text.replace_range(start_idx..end_idx, &new_text);
    }
    Ok(text)
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkspaceTransaction {
    state: String,
    entries: Vec<WorkspaceTransactionEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkspaceTransactionEntry {
    path: String,
    backup: String,
    original_hash: String,
    updated_hash: String,
}

fn transaction_dir(root: &Path) -> PathBuf {
    root.join(LSP_TRANSACTION_DIR)
}

fn reject_link(path: &Path, want_dir: bool) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect transaction path {}", path.display()))?;
    #[cfg(windows)]
    let reparse = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x0000_0400 != 0
    };
    #[cfg(not(windows))]
    let reparse = false;
    if metadata.file_type().is_symlink() || reparse || metadata.is_dir() != want_dir {
        bail!("unsafe transaction path: {}", path.display());
    }
    Ok(())
}

fn safe_transaction_dir(root: &Path, create: bool) -> Result<PathBuf> {
    let root = dunce::canonicalize(root)?;
    let dir = transaction_dir(&root);
    match std::fs::symlink_metadata(&dir) {
        Ok(_) => reject_link(&dir, true)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            std::fs::create_dir(&dir)
                .with_context(|| format!("create transaction directory {}", dir.display()))?;
            crate::providers::restrict_omg_directory_permissions(&dir)?;
            reject_link(&dir, true)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(dir),
        Err(error) => return Err(error.into()),
    }
    Ok(dir)
}

fn transaction_file(dir: &Path) -> PathBuf {
    dir.join("journal.json")
}

fn checked_transaction_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        bail!("invalid transaction target path");
    }
    let path = root.join(relative);
    let canonical = dunce::canonicalize(&path)
        .with_context(|| format!("transaction target is missing: {}", path.display()))?;
    if !canonical.starts_with(root) || canonical.starts_with(transaction_dir(root)) {
        bail!(
            "transaction target is outside workspace: {}",
            path.display()
        );
    }
    let mut current = root.to_path_buf();
    let count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        let Component::Normal(part) = component else {
            unreachable!()
        };
        current.push(part);
        reject_link(&current, index + 1 != count)?;
    }
    Ok(canonical)
}

fn file_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn read_transaction_file(path: &Path) -> Result<Vec<u8>> {
    reject_link(path, false)?;
    let metadata = std::fs::metadata(path)?;
    if metadata.len() as usize > MAX_TRANSACTION_FILE_SIZE {
        bail!("transaction file is too large: {}", path.display());
    }
    std::fs::read(path).with_context(|| format!("read transaction file {}", path.display()))
}

fn write_transaction(dir: &Path, transaction: &WorkspaceTransaction) -> Result<()> {
    let raw = serde_json::to_vec(transaction)?;
    if raw.len() > MAX_TRANSACTION_FILE_SIZE {
        bail!("transaction journal is too large");
    }
    crate::providers::write_file_atomic(&transaction_file(dir), raw, true)
}

fn rollback_transaction(root: &Path, transaction: &WorkspaceTransaction) -> Result<()> {
    let dir = safe_transaction_dir(root, false)?;
    let mut conflicts = Vec::new();
    for entry in transaction.entries.iter().rev() {
        let path = checked_transaction_path(root, &entry.path)?;
        let backup = dir.join(&entry.backup);
        if !matches!(
            Path::new(&entry.backup).components().next(),
            Some(Component::Normal(_))
        ) || Path::new(&entry.backup).components().count() != 1
        {
            bail!("invalid transaction backup path");
        }
        let original = read_transaction_file(&backup)?;
        if file_hash(&original) != entry.original_hash {
            bail!("transaction backup checksum mismatch: {}", backup.display());
        }
        let current = std::fs::read(&path)?;
        if file_hash(&current) == entry.updated_hash {
            crate::providers::write_file_atomic_if_unchanged(&path, &current, &original)?;
        } else if file_hash(&current) != entry.original_hash {
            conflicts.push(path.display().to_string());
        }
    }
    if !conflicts.is_empty() {
        bail!(
            "incomplete workspace edit conflicts with external changes: {}",
            conflicts.join(", ")
        );
    }
    Ok(())
}

fn remove_transaction(root: &Path) -> Result<()> {
    let dir = safe_transaction_dir(root, false)?;
    if !dir.exists() {
        return Ok(());
    }
    reject_link(&dir, true)?;
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        reject_link(&path, false)?;
        std::fs::remove_file(path)?;
    }
    std::fs::remove_dir(&dir)?;
    #[cfg(unix)]
    std::fs::File::open(root)?.sync_all()?;
    Ok(())
}

fn recover_workspace_transaction(root: &Path) -> Result<()> {
    let root = dunce::canonicalize(root)
        .with_context(|| format!("workspace root does not exist: {}", root.display()))?;
    let dir = safe_transaction_dir(&root, false)?;
    let journal = transaction_file(&dir);
    match std::fs::symlink_metadata(&journal) {
        Ok(_) => reject_link(&journal, false)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if dir.exists() {
                remove_transaction(&root)?;
            }
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }
    let raw = read_transaction_file(&journal)?;
    let transaction: WorkspaceTransaction =
        serde_json::from_slice(&raw).context("parse transaction journal")?;
    if !matches!(transaction.state.as_str(), "prepared" | "applying")
        || transaction.entries.is_empty()
        || transaction.entries.len() > MAX_TRANSACTION_ENTRIES
    {
        bail!("invalid incomplete workspace edit journal");
    }
    let mut paths = std::collections::BTreeSet::new();
    let mut backups = std::collections::BTreeSet::new();
    if transaction
        .entries
        .iter()
        .any(|entry| !paths.insert(&entry.path) || !backups.insert(&entry.backup))
    {
        bail!("invalid incomplete workspace edit journal");
    }
    if transaction.state == "applying" {
        rollback_transaction(&root, &transaction)?;
    }
    remove_transaction(&root)
}

fn commit_workspace_edit(root: &Path, writes: Vec<(PathBuf, (String, String))>) -> Result<()> {
    recover_workspace_transaction(root)?;
    if writes.is_empty() {
        bail!("language server produced an empty semantic rename edit");
    }
    if writes.len() > MAX_TRANSACTION_ENTRIES {
        bail!("workspace edit has too many files");
    }
    let dir = safe_transaction_dir(root, true)?;
    let mut entries = Vec::with_capacity(writes.len());
    for (index, (path, (original, updated))) in writes.iter().enumerate() {
        if original.len() > MAX_TRANSACTION_FILE_SIZE || updated.len() > MAX_TRANSACTION_FILE_SIZE {
            bail!(
                "workspace edit file exceeds transaction size limit: {}",
                path.display()
            );
        }
        let relative = path
            .strip_prefix(root)
            .context("workspace edit target is outside root")?;
        let path = checked_transaction_path(root, &relative.to_string_lossy())?;
        let current = std::fs::read_to_string(&path)?;
        if current != *original {
            remove_transaction(root)?;
            bail!(
                "workspace edit source changed before commit at {}",
                path.display()
            );
        }
        let backup = format!("backup-{index}.txt");
        crate::providers::write_file_atomic(&dir.join(&backup), original.as_bytes(), true)?;
        entries.push(WorkspaceTransactionEntry {
            path: relative.to_string_lossy().to_string(),
            backup,
            original_hash: file_hash(original.as_bytes()),
            updated_hash: file_hash(updated.as_bytes()),
        });
    }
    let mut transaction = WorkspaceTransaction {
        state: "prepared".into(),
        entries,
    };
    write_transaction(&dir, &transaction)?;
    transaction.state = "applying".into();
    write_transaction(&dir, &transaction)?;
    for (path, (original, updated)) in &writes {
        let current = std::fs::read_to_string(path)
            .with_context(|| format!("recheck {} before workspace edit", path.display()))?;
        if current != *original {
            let rollback = rollback_transaction(root, &transaction);
            match rollback {
                Ok(()) => {
                    remove_transaction(root)?;
                    bail!(
                        "workspace edit source changed before commit at {}; earlier files were rolled back",
                        path.display()
                    );
                }
                Err(rollback_error) => bail!(
                    "workspace edit source changed before commit at {}; recovery required: {rollback_error}",
                    path.display()
                ),
            };
        }
        if let Err(error) = crate::providers::write_file_atomic_if_unchanged(
            path,
            original.as_bytes(),
            updated.as_bytes(),
        ) {
            let rollback = rollback_transaction(root, &transaction);
            return match rollback {
                Ok(()) => {
                    remove_transaction(root)?;
                    Err(error.context("workspace edit failed; earlier files were rolled back"))
                }
                Err(rollback_error) => Err(error.context(format!(
                    "workspace edit failed; recovery required: {rollback_error}"
                ))),
            };
        }
    }
    remove_transaction(root)
}

async fn apply_workspace_edit(edit: &serde_json::Value, root: &Path) -> Result<()> {
    let root = dunce::canonicalize(root)
        .with_context(|| format!("workspace root does not exist: {}", root.display()))?;
    let mut requested = Vec::new();
    if let Some(changes) = edit.get("changes") {
        let changes = changes
            .as_object()
            .context("workspace changes must be an object")?;
        for (uri, edits_value) in changes {
            requested.push((uri.as_str(), edits_value));
        }
    } else if let Some(document_changes) = edit.get("documentChanges") {
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
                    requested.push((uri, edits));
                } else {
                    bail!("document change is missing edits");
                }
            } else if change.get("kind").is_some() {
                bail!("workspace file operations are not supported in refactor");
            } else {
                bail!("unsupported documentChanges entry");
            }
        }
    } else {
        bail!("workspace edit has neither changes nor documentChanges");
    }

    let mut prepared: std::collections::BTreeMap<std::path::PathBuf, (String, String)> =
        std::collections::BTreeMap::new();
    for (uri, edits) in requested {
        let path = url_to_path(uri, &root)?;
        if let Some((_, current)) = prepared.get_mut(&path) {
            *current = apply_text_edits(current, edits)?;
        } else {
            let original = tokio::fs::read_to_string(&path).await?;
            let updated = apply_text_edits(&original, edits)?;
            prepared.insert(path, (original, updated));
        }
    }

    let prepared: Vec<_> = prepared.into_iter().collect();
    tokio::task::spawn_blocking(move || commit_workspace_edit(&root, prepared))
        .await
        .context("workspace edit transaction task panicked")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_rename_rejects_language_server_errors_and_null_results() {
        let error = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32602, "message": "symbol cannot be renamed"}
        });
        assert!(
            rename_workspace_edit(&error)
                .unwrap_err()
                .to_string()
                .contains("symbol cannot be renamed")
        );
        let no_edit = json!({"jsonrpc": "2.0", "id": 1, "result": null});
        assert!(
            rename_workspace_edit(&no_edit)
                .unwrap_err()
                .to_string()
                .contains("no semantic rename edit")
        );
    }

    #[test]
    fn semantic_rename_accepts_cross_file_only_workspace_edit() {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "changes": {
                    "file:///workspace/other.rs": []
                }
            }
        });
        let edit = rename_workspace_edit(&response).unwrap();
        assert!(edit["changes"].get("file:///workspace/other.rs").is_some());
    }

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

    #[tokio::test]
    async fn invalid_later_workspace_edit_leaves_every_file_unchanged() {
        let temp = tempfile::TempDir::new().unwrap();
        let first = temp.path().join("first.rs");
        let second = temp.path().join("second.rs");
        std::fs::write(&first, "first\n").unwrap();
        std::fs::write(&second, "second\n").unwrap();
        let first_uri = Url::from_file_path(&first).unwrap().to_string();
        let second_uri = Url::from_file_path(&second).unwrap().to_string();
        let edit = serde_json::json!({
            "documentChanges": [
                {
                    "textDocument": {"uri": first_uri},
                    "edits": [{
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 5}
                        },
                        "newText": "changed"
                    }]
                },
                {
                    "textDocument": {"uri": second_uri},
                    "edits": [{
                        "range": {
                            "start": {"line": 0, "character": 4},
                            "end": {"line": 0, "character": 1}
                        },
                        "newText": "invalid"
                    }]
                }
            ]
        });

        assert!(apply_workspace_edit(&edit, temp.path()).await.is_err());
        assert_eq!(std::fs::read_to_string(first).unwrap(), "first\n");
        assert_eq!(std::fs::read_to_string(second).unwrap(), "second\n");
    }

    #[tokio::test]
    async fn workspace_edit_commits_under_a_canonicalized_root() {
        let temp = tempfile::TempDir::new().unwrap();
        let file = temp.path().join("main.rs");
        std::fs::write(&file, "old\n").unwrap();
        let uri = Url::from_file_path(&file).unwrap().to_string();
        let edit = serde_json::json!({
            "changes": {
                (uri): [{
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 3}
                    },
                    "newText": "new"
                }]
            }
        });

        apply_workspace_edit(&edit, temp.path()).await.unwrap();

        assert_eq!(std::fs::read_to_string(file).unwrap(), "new\n");
        assert!(!transaction_dir(temp.path()).exists());
    }

    #[tokio::test]
    async fn unknown_document_change_shape_is_rejected() {
        let temp = tempfile::TempDir::new().unwrap();
        let edit = serde_json::json!({
            "documentChanges": [{"unexpected": true}]
        });
        let error = apply_workspace_edit(&edit, temp.path()).await.unwrap_err();
        assert!(error.to_string().contains("unsupported documentChanges"));
    }

    #[tokio::test]
    async fn empty_workspace_edit_is_rejected_without_leaving_a_journal() {
        let temp = tempfile::TempDir::new().unwrap();
        let error = apply_workspace_edit(&json!({"changes": {}}), temp.path())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("empty semantic rename edit"));
        assert!(!transaction_dir(temp.path()).exists());
    }

    fn incomplete_transaction(root: &Path, path: &Path, original: &str, updated: &str) {
        let root = dunce::canonicalize(root).unwrap();
        let path = dunce::canonicalize(path).unwrap();
        let dir = safe_transaction_dir(&root, true).unwrap();
        let relative = path
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        crate::providers::write_file_atomic(dir.join("backup-0.txt").as_path(), original, true)
            .unwrap();
        write_transaction(
            &dir,
            &WorkspaceTransaction {
                state: "applying".into(),
                entries: vec![WorkspaceTransactionEntry {
                    path: relative,
                    backup: "backup-0.txt".into(),
                    original_hash: file_hash(original.as_bytes()),
                    updated_hash: file_hash(updated.as_bytes()),
                }],
            },
        )
        .unwrap();
    }

    #[test]
    fn incomplete_workspace_edit_is_recovered_before_next_refactor() {
        let temp = tempfile::TempDir::new().unwrap();
        let file = temp.path().join("main.rs");
        std::fs::write(&file, "new\n").unwrap();
        incomplete_transaction(temp.path(), &file, "old\n", "new\n");

        recover_workspace_transaction(temp.path()).unwrap();

        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old\n");
        assert!(!transaction_dir(temp.path()).exists());
    }

    #[test]
    fn prepared_workspace_edit_is_discarded_without_touching_sources() {
        let temp = tempfile::TempDir::new().unwrap();
        let file = temp.path().join("main.rs");
        std::fs::write(&file, "old\n").unwrap();
        incomplete_transaction(temp.path(), &file, "old\n", "new\n");
        let dir = transaction_dir(temp.path());
        let mut transaction: WorkspaceTransaction =
            serde_json::from_slice(&read_transaction_file(&transaction_file(&dir)).unwrap())
                .unwrap();
        transaction.state = "prepared".into();
        write_transaction(&dir, &transaction).unwrap();

        recover_workspace_transaction(temp.path()).unwrap();

        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old\n");
        assert!(!dir.exists());
    }

    #[test]
    fn incomplete_workspace_edit_conflict_fails_closed_and_keeps_journal() {
        let temp = tempfile::TempDir::new().unwrap();
        let file = temp.path().join("main.rs");
        std::fs::write(&file, "external\n").unwrap();
        incomplete_transaction(temp.path(), &file, "old\n", "new\n");

        let error = recover_workspace_transaction(temp.path()).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("conflicts with external changes")
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "external\n");
        assert!(transaction_file(&transaction_dir(temp.path())).exists());
    }

    #[test]
    fn incomplete_workspace_edit_rejects_symlinked_backup() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let temp = tempfile::TempDir::new().unwrap();
            let file = temp.path().join("main.rs");
            let outside = temp.path().join("outside.txt");
            std::fs::write(&file, "new\n").unwrap();
            std::fs::write(&outside, "old\n").unwrap();
            incomplete_transaction(temp.path(), &file, "old\n", "new\n");
            let backup = transaction_dir(temp.path()).join("backup-0.txt");
            std::fs::remove_file(&backup).unwrap();
            symlink(&outside, &backup).unwrap();

            assert!(recover_workspace_transaction(temp.path()).is_err());
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\n");
        }
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
