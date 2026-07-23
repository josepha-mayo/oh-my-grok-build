//! Environment diagnostics and remediation for `omgb`.

use std::collections::HashSet;
use std::io::{IsTerminal, stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::event::{Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone)]
enum Status {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone)]
struct Check {
    name: String,
    status: Status,
    message: String,
    fixed: bool,
}

fn ok(name: &str, message: impl Into<String>) -> Check {
    Check {
        name: name.to_string(),
        status: Status::Ok,
        message: message.into(),
        fixed: false,
    }
}

fn warn(name: &str, message: impl Into<String>) -> Check {
    Check {
        name: name.to_string(),
        status: Status::Warn,
        message: message.into(),
        fixed: false,
    }
}

fn fail(name: &str, message: impl Into<String>) -> Check {
    Check {
        name: name.to_string(),
        status: Status::Fail,
        message: message.into(),
        fixed: false,
    }
}

fn fixed_ok(name: &str, message: impl Into<String>) -> Check {
    Check {
        name: name.to_string(),
        status: Status::Ok,
        message: message.into(),
        fixed: true,
    }
}

async fn run_checks(fix: bool) -> Vec<Check> {
    vec![
        check_providers().unwrap_or_else(|e| fail("provider config", format!("{e:#}"))),
        check_env_permissions(fix).unwrap_or_else(|e| fail("env permissions", format!("{e:#}"))),
        check_safe_shell_guard(fix)
            .await
            .unwrap_or_else(|e| fail("safe-shell-guard", format!("{e:#}"))),
        check_plugin_hooks().unwrap_or_else(|e| fail("plugin hooks", format!("{e:#}"))),
        check_stale_daemons(fix).unwrap_or_else(|e| fail("stale daemons", format!("{e:#}"))),
        check_git()
            .await
            .unwrap_or_else(|e| fail("git", format!("{e:#}"))),
    ]
}

/// Run a full environment health check. If `fix` is true, apply safe remediations.
pub async fn run_doctor(fix: bool, json: bool) -> Result<()> {
    if fix && !json && stdout().is_terminal() {
        return run_doctor_tui().await;
    }
    let checks = run_checks(fix).await;
    print_report(&checks, fix, json);
    Ok(())
}

fn is_fixable(name: &str) -> bool {
    matches!(
        name,
        "env permissions" | "safe-shell-guard" | "stale daemons"
    )
}

async fn apply_fix_by_name(name: &str) -> Result<Check> {
    match name {
        "env permissions" => check_env_permissions(true),
        "safe-shell-guard" => check_safe_shell_guard(true).await,
        "stale daemons" => check_stale_daemons(true),
        _ => bail!("'{name}' has no automated remediation"),
    }
}

async fn run_doctor_tui() -> Result<()> {
    let initial = run_checks(false).await;
    let checks = initial.clone();
    let selected = tokio::task::spawn_blocking(move || tui_select_fixes(&checks))
        .await
        .map_err(|e| anyhow::anyhow!("TUI task panicked: {e}"))??;

    let mut final_checks = initial;
    for name in selected {
        let updated = apply_fix_by_name(&name)
            .await
            .unwrap_or_else(|e| fail(&name, format!("failed to apply remediation: {e:#}")));
        if let Some(c) = final_checks.iter_mut().find(|c| c.name == name) {
            *c = updated;
        }
    }

    print_report(&final_checks, true, false);
    Ok(())
}

fn tui_select_fixes(checks: &[Check]) -> Result<Vec<String>> {
    let fixable: HashSet<String> = checks
        .iter()
        .filter(|c| is_fixable(&c.name) && !matches!(c.status, Status::Ok))
        .map(|c| c.name.clone())
        .collect();
    if fixable.is_empty() {
        return Ok(Vec::new());
    }

    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = ListState::default();
    state.select(Some(0));
    let mut selected: HashSet<String> = fixable.clone();
    let mut apply = false;

    let result = loop {
        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(f.area());

            let items: Vec<ListItem> = checks
                .iter()
                .map(|c| {
                    let can_fix = fixable.contains(&c.name);
                    let checkbox = if can_fix {
                        if selected.contains(&c.name) {
                            "[x]"
                        } else {
                            "[ ]"
                        }
                    } else {
                        "   "
                    };
                    let (glyph, color) = match c.status {
                        Status::Ok => ("[OK]", Color::Green),
                        Status::Warn => ("[WARN]", Color::Yellow),
                        Status::Fail => ("[FAIL]", Color::Red),
                    };
                    let line = Line::from(vec![
                        Span::styled(checkbox, Style::default().fg(Color::Cyan)),
                        Span::raw(" "),
                        Span::styled(glyph, Style::default().fg(color)),
                        Span::raw(format!(" {}: {}", c.name, c.message)),
                    ]);
                    ListItem::new(line)
                })
                .collect();

            let title = if fixable.is_empty() {
                "omgb doctor — no remediations available".to_string()
            } else {
                "omgb doctor — Space=toggle, Enter=apply, q=quit".to_string()
            };
            let list = List::new(items)
                .block(Block::default().title(title).borders(Borders::ALL))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                .highlight_symbol("> ");
            f.render_stateful_widget(list, chunks[0], &mut state);

            let help = Paragraph::new(
                "Use arrow keys to move, Space to toggle a fix, Enter to apply, q to quit.",
            );
            f.render_widget(help, chunks[1]);
        })?;

        if crossterm::event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = crossterm::event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break Vec::new(),
                KeyCode::Up => {
                    if let Some(i) = state.selected() {
                        state.select(Some(i.saturating_sub(1)));
                    }
                }
                KeyCode::Down => {
                    if let Some(i) = state.selected() {
                        state.select(Some((i + 1).min(checks.len() - 1)));
                    }
                }
                KeyCode::Char(' ') => {
                    if let Some(i) = state.selected() {
                        let name = &checks[i].name;
                        if fixable.contains(name) && !selected.insert(name.clone()) {
                            selected.remove(name);
                        }
                    }
                }
                KeyCode::Enter => {
                    apply = true;
                    break selected.into_iter().collect();
                }
                _ => {}
            }
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    if apply { Ok(result) } else { Ok(Vec::new()) }
}

fn print_report(checks: &[Check], fix: bool, json: bool) {
    if json {
        let json = json!({
            "fix": fix,
            "checks": checks.iter().map(|c| json!({
                "name": c.name,
                "status": status_str(&c.status),
                "message": c.message,
                "fixed": c.fixed,
            })).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&json).unwrap_or_default()
        );
    } else {
        for c in checks {
            let glyph = match c.status {
                Status::Ok => "[OK]",
                Status::Warn => "[WARN]",
                Status::Fail => "[FAIL]",
            };
            println!("{glyph} {}: {}", c.name, c.message);
            if c.fixed {
                println!("       (fixed)");
            }
        }
        let failures = checks
            .iter()
            .filter(|c| matches!(c.status, Status::Fail))
            .count();
        let warnings = checks
            .iter()
            .filter(|c| matches!(c.status, Status::Warn))
            .count();
        let fixed = checks.iter().filter(|c| c.fixed).count();
        println!();
        println!("doctor: {failures} failures, {warnings} warnings, {fixed} remediations applied");
    }
}

fn status_str(status: &Status) -> &'static str {
    match status {
        Status::Ok => "ok",
        Status::Warn => "warn",
        Status::Fail => "fail",
    }
}

fn check_providers() -> Result<Check> {
    let cfg = crate::providers::load_omg_config()?;
    let providers: Vec<_> = cfg.providers.values().cloned().collect();
    if providers.is_empty() {
        return Ok(warn("provider config", "no providers configured"));
    }

    let mut issues = Vec::new();
    for p in &providers {
        if p.id.trim().is_empty() {
            issues.push("provider with empty id".to_string());
            continue;
        }
        if p.name.trim().is_empty() {
            issues.push(format!("provider {}: empty name", p.id));
        }
        if p.model.trim().is_empty() {
            issues.push(format!("provider {}: empty model", p.id));
        }
        if p.base_url.trim().is_empty() {
            issues.push(format!("provider {}: empty base_url", p.id));
        } else if !is_http_url(&p.base_url) {
            issues.push(format!("provider {}: base_url is not http(s)", p.id));
        }

        if let Some(ref keys) = p.env_key {
            for k in keys {
                if !crate::providers::is_valid_env_key(k) {
                    issues.push(format!("provider {}: invalid env_key {}", p.id, k));
                }
            }
        }

        if !crate::providers::is_local_provider_id(&p.id) {
            match crate::providers::resolve_api_key(p) {
                Ok(None) | Err(_) => {
                    issues.push(format!("provider {}: API key not resolvable", p.id));
                }
                Ok(Some(_)) => {}
            }
        }
    }

    if issues.is_empty() {
        Ok(ok(
            "provider config",
            format!("{} provider(s) valid", providers.len()),
        ))
    } else {
        Ok(fail(
            "provider config",
            format!(
                "{} provider(s), {} issue(s): {}",
                providers.len(),
                issues.len(),
                issues.join("; ")
            ),
        ))
    }
}

fn is_http_url(raw: &str) -> bool {
    raw.starts_with("http://") || raw.starts_with("https://")
}

fn check_env_permissions(fix: bool) -> Result<Check> {
    let path = crate::providers::omg_dir()?.join(".env");
    if !path.exists() {
        if fix {
            crate::providers::write_file_atomic(&path, "".as_bytes(), true)
                .context("failed to create ~/.omgb/.env")?;
            return Ok(fixed_ok(
                "env permissions",
                "created ~/.omgb/.env with restricted permissions",
            ));
        }
        return Ok(warn("env permissions", "~/.omgb/.env does not exist"));
    }

    if let Some(reason) = env_permissions_not_restricted(&path) {
        if fix {
            let raw = std::fs::read_to_string(&path).unwrap_or_default();
            crate::providers::write_file_atomic(&path, raw.as_bytes(), true)
                .context("failed to rewrite ~/.omgb/.env")?;
            return Ok(fixed_ok(
                "env permissions",
                format!("rewrote ~/.omgb/.env with restricted permissions ({reason})"),
            ));
        }
        return Ok(warn("env permissions", reason));
    }

    Ok(ok(
        "env permissions",
        "~/.omgb/.env has restricted permissions",
    ))
}

#[cfg(unix)]
fn env_permissions_not_restricted(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).ok()?;
    let mode = meta.permissions().mode() & 0o777;
    if mode == 0o600 {
        None
    } else {
        Some(format!("mode is {mode:03o}, expected 600"))
    }
}

#[cfg(windows)]
fn env_permissions_not_restricted(_path: &Path) -> Option<String> {
    // Windows uses ACLs rather than Unix modes; --fix will still enforce the
    // project-standard ACLs via write_file_atomic/restrict_omg_file_permissions.
    None
}

#[cfg(not(any(unix, windows)))]
fn env_permissions_not_restricted(_path: &Path) -> Option<String> {
    None
}

async fn check_safe_shell_guard(fix: bool) -> Result<Check> {
    let root = find_workspace_root().context("cannot locate workspace root")?;
    let plugin_bin = root.join("plugin").join("bin");
    let binary = plugin_bin.join(safe_shell_guard_name());

    if binary.is_file() {
        return Ok(ok(
            "safe-shell-guard",
            format!("found {}", binary.display()),
        ));
    }

    if !fix {
        return Ok(fail(
            "safe-shell-guard",
            format!("missing {}", binary.display()),
        ));
    }

    build_and_copy_safe_shell_guard(&root, &binary)
        .await
        .with_context(|| format!("failed to build safe-shell-guard to {}", binary.display()))?;

    if binary.is_file() {
        Ok(fixed_ok(
            "safe-shell-guard",
            format!("built and copied {}", binary.display()),
        ))
    } else {
        Ok(fail(
            "safe-shell-guard",
            format!("binary not present after build: {}", binary.display()),
        ))
    }
}

fn safe_shell_guard_name() -> &'static str {
    if cfg!(windows) {
        "safe-shell-guard.exe"
    } else {
        "safe-shell-guard"
    }
}

fn find_workspace_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    for _ in 0..6 {
        let cargo = dir.join("Cargo.toml");
        if cargo.is_file()
            && let Ok(text) = std::fs::read_to_string(&cargo)
            && text.contains("[workspace]")
        {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
    None
}

async fn build_and_copy_safe_shell_guard(root: &Path, dst: &Path) -> Result<()> {
    let profile = build_profile();
    let mut cmd = tokio::process::Command::new("cargo");
    cmd.arg("build")
        .arg("-p")
        .arg("oh-my-grok-build")
        .arg("--bin")
        .arg("safe-shell-guard")
        .current_dir(root);
    if profile == "release" {
        cmd.arg("--release");
    }

    let mut child = cmd.spawn().context("failed to spawn cargo build")?;
    let result = tokio::time::timeout(Duration::from_secs(120), child.wait()).await;
    let status = match result {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(anyhow::anyhow!("cargo build failed to run: {e}")),
        Err(_) => {
            let _ = child.kill().await;
            return Err(anyhow::anyhow!("cargo build timed out"));
        }
    };
    if !status.success() {
        return Err(anyhow::anyhow!("cargo build failed with status {status:?}"));
    }

    let src = root
        .join("target")
        .join(profile)
        .join(safe_shell_guard_name());
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&src, dst)
        .with_context(|| format!("failed to copy {} to {}", src.display(), dst.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o755))?;
    }

    Ok(())
}

fn build_profile() -> &'static str {
    if let Ok(exe) = std::env::current_exe() {
        let path = exe.to_string_lossy().to_lowercase();
        if path.contains("/release/") || path.contains("\\release\\") {
            return "release";
        }
    }
    "debug"
}

fn check_plugin_hooks() -> Result<Check> {
    let root = find_workspace_root().context("cannot locate workspace root")?;
    let hooks_json = root.join("plugin").join("hooks").join("hooks.json");
    if !hooks_json.is_file() {
        return Ok(fail(
            "plugin hooks",
            format!("missing {}", hooks_json.display()),
        ));
    }

    let raw = std::fs::read_to_string(&hooks_json)
        .with_context(|| format!("failed to read {}", hooks_json.display()))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not valid JSON", hooks_json.display()))?;
    if value.get("hooks").is_none() {
        return Ok(fail(
            "plugin hooks",
            format!("{} has no top-level hooks object", hooks_json.display()),
        ));
    }

    Ok(ok(
        "plugin hooks",
        format!("{} present", hooks_json.display()),
    ))
}

#[derive(Deserialize)]
struct SubagentRecord {
    #[allow(dead_code)]
    id: String,
    pid: u32,
}

fn check_stale_daemons(fix: bool) -> Result<Check> {
    let omg = crate::providers::omg_dir()?;
    let mut messages = Vec::new();

    // Scheduler PID file.
    let scheduler_pid = omg.join("scheduler.pid");
    if scheduler_pid.is_file() {
        let raw = std::fs::read_to_string(&scheduler_pid).unwrap_or_default();
        let pid = raw.trim().parse::<u32>();
        match pid {
            Ok(pid) if crate::process_alive(pid) => {
                messages.push(format!("scheduler running (pid {pid})"));
            }
            _ => {
                if fix {
                    let _ = std::fs::remove_file(&scheduler_pid);
                    messages.push("removed stale scheduler.pid".to_string());
                } else {
                    messages.push(format!("stale scheduler.pid: {}", raw.trim()));
                }
            }
        }
    }

    // Subagent registry.
    let subagents_path = omg.join("subagents.jsonl");
    if subagents_path.is_file() {
        let raw = std::fs::read_to_string(&subagents_path).unwrap_or_default();
        let mut alive = Vec::new();
        let mut stale = 0;
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<SubagentRecord>(trimmed) {
                Ok(rec) if crate::process_alive(rec.pid) => alive.push(line.to_string()),
                _ => stale += 1,
            }
        }

        if stale > 0 {
            if fix {
                let content = alive.join("\n");
                if content.is_empty() {
                    let _ = std::fs::remove_file(&subagents_path);
                    messages.push("removed subagents.jsonl (all stale)".to_string());
                } else {
                    crate::providers::write_file_atomic(&subagents_path, content.as_bytes(), true)
                        .context("failed to rewrite subagents.jsonl")?;
                    messages.push(format!("removed {stale} stale subagent record(s)"));
                }
            } else {
                messages.push(format!("{stale} stale subagent record(s)"));
            }
        } else {
            messages.push(format!("{} subagent record(s) alive", alive.len()));
        }
    }

    if messages.is_empty() {
        Ok(ok("stale daemons", "no scheduler/subagent state found"))
    } else {
        let has_stale = messages.iter().any(|m| m.contains("stale"));
        if has_stale && !fix {
            Ok(warn("stale daemons", messages.join("; ")))
        } else {
            Ok(ok("stale daemons", messages.join("; ")))
        }
    }
}

async fn check_git() -> Result<Check> {
    let output = tokio::process::Command::new("git")
        .arg("--version")
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            Ok(ok("git", text.trim().to_string()))
        }
        _ => Ok(fail("git", "git is not installed or not on PATH")),
    }
}
