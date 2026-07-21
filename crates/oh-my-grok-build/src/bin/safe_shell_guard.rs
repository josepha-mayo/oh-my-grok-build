#![allow(clippy::too_many_lines)]

use serde_json::{Value, json};
use std::io::{self, Read};

const IS_WINDOWS: bool = cfg!(windows);

const DANGEROUS_COMMANDS: &[&str] = &[
    "mkfs", "fdisk", "diskpart", "format", "shutdown", "reboot", "halt", "poweroff",
];

const UNANALYZABLE_COMMANDS: &[&str] = &[
    "python",
    "python2",
    "python3",
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
    "script",
    "screen",
    "tmux",
    "expect",
    "powershell",
    "pwsh",
    "powershell.exe",
    "pwsh.exe",
    "eval",
    "source",
    ".",
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
    "wscript",
    "cscript",
    "mshta",
];

const WINDOWS_CMD_LAUNCHERS: &[&str] = &["start", "call", "runas"];

const SHELL_METACHARS: &[char] = &[';', '&', '|', '>', '<', '(', ')', '{', '}', '!', '\n', '\r'];

const NORMALIZABLE_STEMS: &[&str] = &[
    "python",
    "python2",
    "python3",
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
    ".bat", ".cmd", ".ps1", ".vbs", ".js", ".wsf", ".py", ".pl", ".rb", ".sh", ".hta", ".scr",
    ".pif", ".cpl", ".msc",
];

fn win_exec_ext(s: &str) -> Option<&str> {
    WIN_EXEC_EXTS
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
        if SHELL_METACHARS.contains(&ch) {
            return Err(format!("Blocked shell metacharacter '{}'", ch));
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
    let mut chars = token.value.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    for c in chars {
        if c == '=' {
            return true;
        }
        if !c.is_ascii_alphanumeric() && c != '_' {
            return false;
        }
    }
    false
}

fn has_bare_tilde(token: &Token) -> bool {
    token.quoted.is_none() && token.value.contains('~')
}

fn evaluate_tokens(tokens: &[Token], start: usize) -> Decision {
    if start >= tokens.len() {
        return Decision::Deny("Empty command".into());
    }
    let mut i = start;
    while i < tokens.len() && is_assignment(&tokens[i]) {
        i += 1;
    }
    if i >= tokens.len() {
        return Decision::Allow;
    }
    match skip_prefix(tokens, i) {
        PrefixOutcome::Stop(decision) => decision,
        PrefixOutcome::Continue(idx) => {
            i = idx;
            if i >= tokens.len() {
                return Decision::Allow;
            }
            let cmd_token = &tokens[i];
            let base = get_base_name(&cmd_token.value);

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
                return Decision::Deny(format!(
                    "Blocked potentially destructive command: {}",
                    base
                ));
            }

            if let Some(ext) = win_exec_ext(&cmd_token.value)
                && SCRIPT_EXTS.contains(&ext)
                && !UNANALYZABLE_COMMANDS.contains(&base.as_str())
                && !DANGEROUS_COMMANDS.contains(&base.as_str())
            {
                return Decision::Deny(format!(
                    "Blocked unanalyzable script file: {}",
                    cmd_token.value
                ));
            }

            if base == "dd" {
                let argv: Vec<&str> = tokens[i..].iter().map(|t| t.value.as_str()).collect();
                if argv.iter().any(|a| a.to_lowercase().contains("of=/dev/")) {
                    return Decision::Deny("Blocked dd writing to a raw device".into());
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
                        && "fFsSqQaA".contains(a.chars().nth(1).unwrap())
                }) {
                    return Decision::Deny("Blocked destructive Windows delete".into());
                }
                return Decision::Allow;
            }
            if base == "rd" || base == "rmdir" {
                let argv: Vec<&str> = tokens[i..].iter().map(|t| t.value.as_str()).collect();
                if argv.iter().any(|a| {
                    a.len() >= 2 && a.starts_with('/') && "sSqQ".contains(a.chars().nth(1).unwrap())
                }) {
                    return Decision::Deny("Blocked destructive Windows rd/rmdir".into());
                }
                return Decision::Allow;
            }
            Decision::Allow
        }
    }
}

fn skip_prefix(tokens: &[Token], i: usize) -> PrefixOutcome {
    if i >= tokens.len() {
        return PrefixOutcome::Continue(i);
    }
    let base = get_base_name(&tokens[i].value);
    match base.as_str() {
        "nohup" | "setsid" => PrefixOutcome::Continue(i + 1),
        "nice" => parse_nice(tokens, i),
        "env" => parse_env(tokens, i),
        "timeout" => parse_timeout(tokens, i),
        "stdbuf" => parse_stdbuf(tokens, i),
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
        if v.starts_with('-') && v != "--" {
            j += 1;
            continue;
        }
        break;
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
        if v.starts_with('-') && v != "--" {
            return PrefixOutcome::Stop(Decision::Deny("Blocked unknown timeout option".into()));
        }
        if v == "--" {
            return PrefixOutcome::Continue(j + 1);
        }
        break;
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
    let value_short: &[char] = &['u', 'g', 'p', 'r', 't', 'C', 'U', 'D', 'c', 'T'];
    let value_long: &[&str] = &[
        "user",
        "group",
        "prompt",
        "role",
        "type",
        "close-from",
        "other-user",
        "chdir",
        "command",
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
            return PrefixOutcome::Stop(evaluate(&tokens[j + 1].value));
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
            if j + 2 == tokens.len() && tokens[j + 1].quoted.is_some() {
                let inner = match tokenize(&tokens[j + 1].value) {
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
            let first = first_command_token(tokens, j + 1);
            if let Some((_, ref base)) = first
                && WINDOWS_CMD_LAUNCHERS.contains(&base.as_str())
            {
                return PrefixOutcome::Stop(Decision::Deny(format!(
                    "Blocked unanalyzable command: {}",
                    base
                )));
            }
            return PrefixOutcome::Stop(evaluate_tokens(
                tokens,
                first.map(|(idx, _)| idx).unwrap_or(tokens.len()),
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
        if recursive && force && is_dangerous_rm_target(arg) {
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

fn is_dangerous_rm_target(arg: &str) -> bool {
    let mut target = arg.to_string();
    if let Some(t) = target.strip_prefix('"') {
        target = t.to_string();
    }
    if let Some(t) = target.strip_prefix('\'') {
        target = t.to_string();
    }
    if let Some(t) = target.strip_suffix('"') {
        target = t.to_string();
    }
    if let Some(t) = target.strip_suffix('\'') {
        target = t.to_string();
    }

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
        if normalized.to_lowercase() == home_norm.to_lowercase() {
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

    if target.contains('*') {
        let dir_part = target.split('*').next().unwrap_or("");
        let norm_dir = normalize_target(dir_part);
        if norm_dir == sep.to_string() || norm_dir == "/" || norm_dir == "\\" {
            return true;
        }
        if norm_dir.len() == 3
            && norm_dir.as_bytes()[0].is_ascii_alphabetic()
            && norm_dir.as_bytes()[1] == b':'
            && (norm_dir.as_bytes()[2] == b'\\' || norm_dir.as_bytes()[2] == b'/')
        {
            return true;
        }
        let clean_dir = norm_dir.trim_end_matches(['/', '\\']);
        if !home.is_empty() {
            let home_norm = normalize_target(&home);
            if clean_dir.to_lowercase() == home_norm.to_lowercase() {
                return true;
            }
        }
        let ends_with_sep = dir_part.ends_with('/') || dir_part.ends_with('\\');
        if !ends_with_sep && is_absolute_target(&norm_dir) {
            let dir_segs: Vec<&str> = norm_dir
                .split(['/', '\\'])
                .filter(|s| !s.is_empty())
                .collect();
            if dir_segs.len() <= 2 {
                return true;
            }
        }
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
            ("cmd /c foo.js", "Blocked unanalyzable script file:"),
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
}
