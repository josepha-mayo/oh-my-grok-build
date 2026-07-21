#!/usr/bin/env node
const path = require("node:path");

const IS_WINDOWS = process.platform === "win32";

const input = [];
process.stdin.on("data", (d) => input.push(d));
process.stdin.on("end", () => {
  let payload = {};
  try {
    payload = JSON.parse(input.join(""));
  } catch {
    deny("Invalid guard payload");
    return;
  }

  const cmd = payload.toolInput?.command ?? "";
  if (typeof cmd !== "string") {
    deny("Command must be a string");
    return;
  }

  const result = evaluate(cmd);
  if (result.allowed) {
    allow();
  } else {
    deny(result.reason);
  }
});

function allow() {
  console.log(JSON.stringify({ decision: "allow" }));
  process.exit(0);
}

function deny(reason) {
  console.log(JSON.stringify({ decision: "deny", reason }));
  process.exit(2);
}

const DANGEROUS_COMMANDS = new Set([
  "mkfs",
  "fdisk",
  "diskpart",
  "format",
  "shutdown",
  "reboot",
  "halt",
  "poweroff",
]);

const UNANALYZABLE_COMMANDS = new Set([
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
]);

// Windows cmd.exe builtins that launch other programs; the guard cannot
// determine whether the launched program is safe, so block them only when
// they appear as the effective command in a `cmd /c` / `cmd /k` context.
const WINDOWS_CMD_LAUNCHERS = new Set(["start", "call", "runas"]);

// Characters that separate commands or redirect I/O at the shell level.
const SHELL_METACHARS = new Set([
  ";",
  "&",
  "|",
  ">",
  "<",
  "(",
  ")",
  "{",
  "}",
  "!",
  "\n",
  "\r",
]);

function tokenize(command) {
  const tokens = [];
  let current = "";
  let quote = null; // " or '
  let escape = false;

  for (let i = 0; i < command.length; i++) {
    const ch = command[i];

    if (escape) {
      current += ch;
      escape = false;
      continue;
    }

    if (IS_WINDOWS && !quote && ch === "^") {
      // cmd.exe escape character.
      escape = true;
      continue;
    }

    if (ch === "\\" && quote !== "'") {
      if (IS_WINDOWS) {
        // On Windows backslash is a path separator (PowerShell uses backtick for escape).
        current += ch;
        continue;
      }
      // In double quotes, backslash only escapes $ ` " \ newline.
      if (quote === '"') {
        const next = command[i + 1];
        if (next === "$" || next === "`" || next === '"' || next === "\\" || next === "\n") {
          escape = true;
          continue;
        }
        // Otherwise backslash is literal in double quotes.
        current += ch;
        continue;
      }
      escape = true;
      continue;
    }

    if (quote) {
      if (ch === quote) {
        const kind = quote === "'" ? "single" : "double";
        quote = null;
        tokens.push({ value: current, quoted: kind });
        current = "";
      } else if (quote === '"' && (ch === "$" || ch === "`" || (IS_WINDOWS && ch === "%"))) {
        let reason;
        if (ch === "`") reason = "Blocked command substitution (backtick) inside double quotes";
        else if (ch === "%") reason = "Blocked Windows variable expansion inside double quotes";
        else if (command[i + 1] === "(") reason = "Blocked command substitution inside double quotes";
        else if (command[i + 1] === "{") reason = "Blocked parameter expansion inside double quotes";
        else reason = "Blocked variable expansion inside double quotes";
        return { error: reason };
      } else {
        current += ch;
      }
      continue;
    }

    if (ch === '"' || ch === "'") {
      if (current) {
        tokens.push({ value: current, quoted: null });
        current = "";
      }
      quote = ch;
      continue;
    }

    if (/\s/.test(ch)) {
      if (current) {
        tokens.push({ value: current, quoted: null });
        current = "";
      }
      continue;
    }

    if (SHELL_METACHARS.has(ch)) {
      return { error: `Blocked shell metacharacter '${ch}'` };
    }

    if (ch === "$" || ch === "`" || (IS_WINDOWS && ch === "%")) {
      if (ch === "`") return { error: "Blocked command substitution (backtick)" };
      if (ch === "%") return { error: "Blocked Windows variable expansion" };
      if (command[i + 1] === "(") return { error: "Blocked command substitution" };
      if (command[i + 1] === "{") return { error: "Blocked parameter expansion" };
      return { error: "Blocked variable expansion" };
    }

    current += ch;
  }

  if (quote !== null) {
    return { error: "Unterminated quoted string" };
  }

  if (escape) {
    return { error: "Trailing backslash escape" };
  }

  if (current) {
    tokens.push({ value: current, quoted: null });
  }

  return { tokens };
}

// Stems that commonly appear with a trailing version number (e.g. python3.11,
// ksh93, node22). getBaseName normalizes these back to the canonical stem so
// versioned interpreter aliases are still caught by the block/prefix lists.
const NORMALIZABLE_STEMS = new Set([
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
]);

function normalizeBaseName(base) {
  let prev;
  do {
    prev = base;
    const dotMatch = base.match(/^(.*)\.\d+$/);
    if (dotMatch && NORMALIZABLE_STEMS.has(dotMatch[1])) {
      base = dotMatch[1];
      continue;
    }
    const numMatch = base.match(/^(.*\D)(\d+(?:\.\d+)*)$/);
    if (numMatch && numMatch[1] && NORMALIZABLE_STEMS.has(numMatch[1])) {
      base = numMatch[1];
      continue;
    }
  } while (base !== prev);
  return base;
}

// Windows executable/script extensions that should be stripped before matching
// dangerous/unanalyzable command lists.  This prevents format.com, node.bat,
// python.cmd, etc. from bypassing the guard.
const WIN_EXEC_EXTENSIONS =
  /\.(exe|com|bat|cmd|ps1|vbs|js|wsf|msc|cpl|scr|pif)$/i;

function getBaseName(cmd) {
  let s = cmd.replace(/^\s+|\s+$/g, "");
  s = s.replace(WIN_EXEC_EXTENSIONS, "");
  const parts = s.split(/[\/\\]/);
  return normalizeBaseName(parts[parts.length - 1]?.toLowerCase() ?? "");
}

function isAssignment(token) {
  return !token.quoted && /^[A-Za-z_][A-Za-z0-9_]*=/.test(token.value);
}

// env -S/--split-string takes a command-line string, splits it by whitespace,
// and executes the result. Reconstruct the full token list and evaluate it.
function getEnvSplitStringArg(tokens, j) {
  const v = tokens[j].value;
  let value;
  let remainingStart;

  if (v === "-S" || v === "--split-string") {
    if (j + 1 >= tokens.length) {
      return { allowed: false, reason: "env -S/--split-string requires a command string" };
    }
    value = tokens[j + 1].value;
    remainingStart = j + 2;
  } else if (v.startsWith("--split-string=")) {
    const eq = "--split-string=".length;
    value = v.slice(eq);
    if (value === "") {
      if (j + 1 >= tokens.length) {
        return { allowed: false, reason: "env --split-string requires a command string" };
      }
      value = tokens[j + 1].value;
      remainingStart = j + 2;
    } else {
      remainingStart = j + 1;
    }
  } else if (v.startsWith("-S") && v.length > 2) {
    value = v.slice(2);
    remainingStart = j + 1;
  } else {
    return null;
  }

  const inner = tokenize(value);
  if (inner.error) {
    return { allowed: false, reason: inner.error };
  }
  const combined = inner.tokens.slice();
  for (let k = remainingStart; k < tokens.length; k++) {
    combined.push({ value: tokens[k].value, quoted: tokens[k].quoted });
  }
  return evaluateTokens(combined, 0);
}

function hasBareTilde(token) {
  return !token.quoted && token.value.includes("~");
}

function evaluate(command) {
  const tokens = tokenize(command);
  if (tokens.error) {
    return { allowed: false, reason: tokens.error };
  }
  return evaluateTokens(tokens.tokens, 0);
}

function evaluateTokens(tokens, start) {
  if (start >= tokens.length) {
    return { allowed: false, reason: "Empty command" };
  }

  // Skip leading environment assignments.
  let i = start;
  while (i < tokens.length && isAssignment(tokens[i])) {
    i++;
  }
  if (i >= tokens.length) {
    return { allowed: true };
  }

  const prefix = skipPrefix(tokens, i);
  if (!prefix.allowed) {
    return prefix;
  }
  if (prefix.index === undefined) {
    // Prefix handler already evaluated the effective command (e.g. bash -c).
    return { allowed: prefix.allowed, reason: prefix.reason };
  }
  i = prefix.index;
  if (i >= tokens.length) {
    return { allowed: true };
  }

  const cmdToken = tokens[i];
  const base = getBaseName(cmdToken.value);

  // Tilde expansion is a variable-like shortcut to the user's home directory.
  // Allow it only for rm (which has its own path-aware check) and cd (which
  // only changes the working directory); for all other commands it can read or
  // write files such as ~/.ssh/id_rsa.
  if (base !== "rm" && base !== "cd") {
    for (let k = i; k < tokens.length; k++) {
      if (hasBareTilde(tokens[k])) {
        return { allowed: false, reason: "Blocked tilde expansion outside quotes" };
      }
    }
  }

  // Fork bomb pattern (classic bash form). Tokenization already blocks ; | &,
  // but the command itself may be passed as a string to bash -c.
  if (/:\(\)\s*\{\s*:\|:\s*&\s*\}\s*;\s*:/.test(cmdToken.value)) {
    return { allowed: false, reason: "Blocked fork bomb" };
  }

  if (UNANALYZABLE_COMMANDS.has(base)) {
    return { allowed: false, reason: `Blocked unanalyzable command: ${base}` };
  }

  if (DANGEROUS_COMMANDS.has(base)) {
    return { allowed: false, reason: `Blocked potentially destructive command: ${base}` };
  }

  if (base === "dd") {
    const argv = tokens.slice(i).map((t) => t.value);
    if (argv.some((a) => /of=\/dev\//i.test(a))) {
      return { allowed: false, reason: "Blocked dd writing to a raw device" };
    }
    return { allowed: true };
  }

  if (base === "rm") {
    return checkRm(tokens, i);
  }

  if (base === "find") {
    return checkFind(tokens, i);
  }

  // Windows destructive commands.
  if (base === "del" || base === "erase") {
    const argv = tokens.slice(i).map((t) => t.value);
    if (argv.some((a) => /^\/[fFsSqQaA]/.test(a))) {
      return { allowed: false, reason: "Blocked destructive Windows delete" };
    }
    return { allowed: true };
  }

  if (base === "rd" || base === "rmdir") {
    const argv = tokens.slice(i).map((t) => t.value);
    if (argv.some((a) => /^\/[sSqQ]/.test(a))) {
      return { allowed: false, reason: "Blocked destructive Windows rd/rmdir" };
    }
    return { allowed: true };
  }

  return { allowed: true };
}

function skipPrefix(tokens, i) {
  if (i >= tokens.length) {
    return { allowed: true, index: i };
  }
  const base = getBaseName(tokens[i].value);
  const handler = PREFIX_HANDLERS[base];
  if (!handler) {
    return { allowed: true, index: i };
  }
  return handler(tokens, i);
}

const PREFIX_HANDLERS = {
  nohup: (_tokens, i) => ({ allowed: true, index: i + 1 }),
  setsid: (_tokens, i) => ({ allowed: true, index: i + 1 }),

  nice: (tokens, i) => {
    let j = i + 1;
    while (j < tokens.length) {
      const v = tokens[j].value;
      if (v === "-n" || v === "--adjustment") {
        if (j + 1 >= tokens.length) return { allowed: false, reason: "nice option requires value" };
        j += 2;
        continue;
      }
      if (v.startsWith("--adjustment=")) {
        j++;
        continue;
      }
      if (v.startsWith("-n") && v.length > 2) {
        j++;
        continue;
      }
      if (v.startsWith("-") && v !== "--") {
        j++;
        continue;
      }
      break;
    }
    return { allowed: true, index: j };
  },

  env: (tokens, i) => {
    const noArg = new Set(["-i", "-v", "--ignore-environment", "--debug", "--help", "--version"]);
    let j = i + 1;
    while (j < tokens.length) {
      const t = tokens[j];
      if (isAssignment(t)) {
        j++;
        continue;
      }
      const v = t.value;
      if (v === "-u" || v === "--unset") {
        if (j + 1 >= tokens.length) return { allowed: false, reason: "env option requires value" };
        j += 2;
        continue;
      }
      if (v.startsWith("--unset=")) {
        j++;
        continue;
      }
      if (v.startsWith("-u") && v.length > 2) {
        j++;
        continue;
      }
      const split = getEnvSplitStringArg(tokens, j);
      if (split !== null) {
        return split;
      }
      if (noArg.has(v)) {
        j++;
        continue;
      }
      if (v.startsWith("-") && v !== "--") {
        return { allowed: false, reason: "Blocked unknown env option" };
      }
      if (v === "--") {
        return { allowed: true, index: j + 1 };
      }
      break;
    }
    return { allowed: true, index: j };
  },

  timeout: (tokens, i) => {
    const noArg = new Set(["--preserve-status", "--foreground", "-v", "--verbose", "--help", "--version"]);
    let j = i + 1;
    while (j < tokens.length) {
      const v = tokens[j].value;
      if (v === "-s" || v === "--signal" || v === "-k" || v === "--kill-after") {
        if (j + 1 >= tokens.length) return { allowed: false, reason: "timeout option requires value" };
        j += 2;
        continue;
      }
      if (v.startsWith("--signal=") || v.startsWith("--kill-after=")) {
        j++;
        continue;
      }
      if ((v.startsWith("-s") || v.startsWith("-k")) && v.length > 2) {
        j++;
        continue;
      }
      if (noArg.has(v)) {
        j++;
        continue;
      }
      if (v.startsWith("-") && v !== "--") {
        return { allowed: false, reason: "Blocked unknown timeout option" };
      }
      if (v === "--") {
        return { allowed: true, index: j + 1 };
      }
      break;
    }
    return { allowed: true, index: j };
  },

  stdbuf: (tokens, i) => {
    let j = i + 1;
    while (j < tokens.length) {
      const v = tokens[j].value;
      if (v === "-i" || v === "-o" || v === "-e" || v === "--input" || v === "--output" || v === "--error") {
        if (j + 1 >= tokens.length) return { allowed: false, reason: "stdbuf option requires value" };
        j += 2;
        continue;
      }
      if (v.startsWith("--input=") || v.startsWith("--output=") || v.startsWith("--error=")) {
        j++;
        continue;
      }
      if ((v.startsWith("-i") || v.startsWith("-o") || v.startsWith("-e")) && v.length > 2) {
        j++;
        continue;
      }
      if (["--help", "--version"].includes(v)) {
        j++;
        continue;
      }
      if (v.startsWith("-") && v !== "--") {
        return { allowed: false, reason: "Blocked unknown stdbuf option" };
      }
      if (v === "--") {
        return { allowed: true, index: j + 1 };
      }
      break;
    }
    return { allowed: true, index: j };
  },

  sudo: (tokens, i) => parseSudoDoas(tokens, i, "sudo"),
  doas: (tokens, i) => parseSudoDoas(tokens, i, "doas"),

  // Interpreters with a command-string argument.
  bash: (tokens, i) => parseInterpreter(tokens, i, "-c"),
  sh: (tokens, i) => parseInterpreter(tokens, i, "-c"),
  dash: (tokens, i) => parseInterpreter(tokens, i, "-c"),
  zsh: (tokens, i) => parseInterpreter(tokens, i, "-c"),
  ksh: (tokens, i) => parseInterpreter(tokens, i, "-c"),
  csh: (tokens, i) => parseInterpreter(tokens, i, "-c"),
  tcsh: (tokens, i) => parseInterpreter(tokens, i, "-c"),
  fish: (tokens, i) => parseInterpreter(tokens, i, "-c"),

  cmd: (tokens, i) => parseCmd(tokens, i),
  "cmd.exe": (tokens, i) => parseCmd(tokens, i),

  wsl: (tokens, i) => parseWsl(tokens, i),

  // busybox is a multi-call binary: the first non-option token is the applet
  // and the rest are its arguments. Evaluate it as that command.
  busybox: (tokens, i) => {
    let j = i + 1;
    while (j < tokens.length && tokens[j].value.startsWith("-")) {
      j++;
    }
    if (j >= tokens.length) {
      return { allowed: false, reason: "Busybox applet not specified" };
    }
    return evaluateTokens(tokens, j);
  },

  xargs: (tokens, i) => parseXargs(tokens, i),
};

function parseSudoDoas(tokens, i, _kind) {
  const valueShort = new Set(["u", "g", "p", "r", "t", "C", "U", "D", "c", "T"]);
  const valueLong = new Set([
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
  ]);
  let j = i + 1;
  while (j < tokens.length) {
    const v = tokens[j].value;
    if (v === "--") {
      return { allowed: true, index: j + 1 };
    }
    if (v.startsWith("--")) {
      const eq = v.indexOf("=");
      const name = eq === -1 ? v.slice(2) : v.slice(2, eq);
      if (valueLong.has(name) && eq === -1) {
        if (j + 1 >= tokens.length) return { allowed: false, reason: `${_kind} option requires value` };
        j += 2;
      } else {
        j++;
      }
      continue;
    }
    if (v.startsWith("-") && v.length > 1) {
      const rest = v.slice(1);
      let valueInNext = false;
      for (let k = 0; k < rest.length; k++) {
        if (valueShort.has(rest[k])) {
          if (k < rest.length - 1) {
            // value is the rest of this option cluster
            break;
          }
          valueInNext = true;
          break;
        }
      }
      if (valueInNext) {
        if (j + 1 >= tokens.length) return { allowed: false, reason: `${_kind} option requires value` };
        j += 2;
      } else {
        j++;
      }
      continue;
    }
    break;
  }
  return { allowed: true, index: j };
}

function parseInterpreter(tokens, i, cmdFlag) {
  let j = i + 1;
  while (j < tokens.length) {
    const v = tokens[j].value;
    if (v === cmdFlag) {
      if (j + 1 >= tokens.length) return { allowed: false, reason: "Interpreter missing command string" };
      return evaluate(tokens[j + 1].value);
    }
    if (v === "--") {
      // End of interpreter options, but no -c found.
      return { allowed: false, reason: "Blocked interpreter without -c command string" };
    }
    if (v.startsWith("-") && v.length > 1) {
      j++;
      continue;
    }
    break;
  }
  return { allowed: false, reason: "Blocked interpreter without -c command string" };
}

// Find the first non-empty, non-option token in a `cmd /c` or `/k` command
// line after cmd.exe strips leading empty quoted strings. Returns the index
// and normalized base name, or null if none.
function firstCommandToken(tokens, start) {
  for (let k = start; k < tokens.length; k++) {
    const v = tokens[k].value;
    if (v.startsWith("/") && v.length > 1) continue;
    const base = getBaseName(v);
    if (!base) continue;
    return { index: k, base };
  }
  return null;
}

function parseCmd(tokens, i) {
  let j = i + 1;
  while (j < tokens.length) {
    const v = tokens[j].value;
    if (v.toLowerCase() === "/c" || v.toLowerCase() === "/k") {
      if (j + 1 >= tokens.length) return { allowed: false, reason: "cmd missing command string" };
      if (j + 2 === tokens.length && tokens[j + 1].quoted !== null) {
        const inner = tokenize(tokens[j + 1].value);
        if (inner.error) return { allowed: false, reason: inner.error };
        const first = firstCommandToken(inner.tokens, 0);
        if (first && WINDOWS_CMD_LAUNCHERS.has(first.base)) {
          return { allowed: false, reason: `Blocked unanalyzable command: ${first.base}` };
        }
        return evaluateTokens(inner.tokens, first ? first.index : inner.tokens.length);
      }
      const first = firstCommandToken(tokens, j + 1);
      if (first && WINDOWS_CMD_LAUNCHERS.has(first.base)) {
        return { allowed: false, reason: `Blocked unanalyzable command: ${first.base}` };
      }
      return evaluateTokens(tokens, first ? first.index : tokens.length);
    }
    if (v.startsWith("/") && v.length > 1) {
      j++;
      continue;
    }
    break;
  }
  return { allowed: false, reason: "Blocked cmd without /c or /k command string" };
}

function parseWsl(tokens, i) {
  const valueOpts = new Set(["-d", "-u", "--distribution", "--user", "--shell", "--cd"]);
  let j = i + 1;
  while (j < tokens.length) {
    const v = tokens[j].value;
    if (v === "--") {
      if (j + 2 === tokens.length && tokens[j + 1].quoted !== null) {
        return evaluate(tokens[j + 1].value);
      }
      return { allowed: true, index: j + 1 };
    }
    if (v === "-e" || v === "--exec") {
      if (j + 1 >= tokens.length) return { allowed: false, reason: "wsl -e/--exec requires command" };
      if (j + 2 === tokens.length && tokens[j + 1].quoted !== null) {
        return evaluate(tokens[j + 1].value);
      }
      return evaluateTokens(tokens, j + 1);
    }
    if (valueOpts.has(v)) {
      if (j + 1 >= tokens.length) return { allowed: false, reason: "wsl option requires value" };
      j += 2;
      continue;
    }
    if (v.startsWith("--") && v.includes("=")) {
      j++;
      continue;
    }
    if (v.startsWith("-") && v.length > 1) {
      j++;
      continue;
    }
    break;
  }
  if (j >= tokens.length) {
    return { allowed: false, reason: "Blocked wsl without command" };
  }
  if (j + 1 === tokens.length && tokens[j].quoted !== null) {
    return evaluate(tokens[j].value);
  }
  return evaluateTokens(tokens, j);
}

function parseXargs(tokens, i) {
  const valueOpts = new Set([
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
  ]);
  const noArg = new Set([
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
  ]);
  let j = i + 1;
  let sawReplace = false;
  while (j < tokens.length) {
    const v = tokens[j].value;
    if (v === "--") {
      j++;
      continue;
    }
    if (v === "-I" || v === "-i" || v === "--replace") {
      sawReplace = true;
      if (j + 1 >= tokens.length) return { allowed: false, reason: "xargs replacement option requires value" };
      if (hasBareTilde(tokens[j + 1])) {
        return { allowed: false, reason: "Blocked tilde expansion in xargs option value" };
      }
      j += 2;
      continue;
    }
    if (v.startsWith("--replace=")) {
      sawReplace = true;
      const eq = v.indexOf("=");
      if (hasBareTilde({ value: v.slice(eq + 1), quoted: null })) {
        return { allowed: false, reason: "Blocked tilde expansion in xargs option value" };
      }
      j++;
      continue;
    }
    if ((v.startsWith("-I") || v.startsWith("-i")) && v.length > 2) {
      sawReplace = true;
      j++;
      continue;
    }
    if (
      v === "-e" ||
      v === "--eof" ||
      (v.startsWith("-e") && v.length > 2) ||
      v.startsWith("--eof=")
    ) {
      j++;
      continue;
    }
    if (noArg.has(v)) {
      j++;
      continue;
    }
    if (valueOpts.has(v)) {
      if (j + 1 >= tokens.length) return { allowed: false, reason: "xargs option requires value" };
      if (hasBareTilde(tokens[j + 1])) {
        return { allowed: false, reason: "Blocked tilde expansion in xargs option value" };
      }
      j += 2;
      continue;
    }
    if (v.startsWith("--") && v.includes("=")) {
      const eq = v.indexOf("=");
      if (hasBareTilde({ value: v.slice(eq + 1), quoted: null })) {
        return { allowed: false, reason: "Blocked tilde expansion in xargs option value" };
      }
      j++;
      continue;
    }
    if (v.startsWith("-") && v.length > 1) {
      return { allowed: false, reason: "Blocked unknown xargs option" };
    }
    break;
  }
  if (sawReplace) {
    return { allowed: false, reason: "Blocked xargs argument replacement (-I/-i/--replace); cannot verify substituted arguments" };
  }
  if (j >= tokens.length) {
    // xargs with no command defaults to echo; that is safe.
    return { allowed: true };
  }
  const cmdBase = getBaseName(tokens[j].value);
  if (DANGEROUS_COMMANDS.has(cmdBase) || UNANALYZABLE_COMMANDS.has(cmdBase)) {
    return { allowed: false, reason: `Blocked xargs with dangerous/unanalyzable command: ${cmdBase}` };
  }
  // xargs appends arbitrary arguments from stdin or a file at runtime, so only
  // allow commands that are safe with arbitrary trailing arguments. printf is
  // not safe because the first stdin argument becomes an unsanitized format string.
  if (cmdBase !== "echo") {
    return { allowed: false, reason: `Blocked xargs with unanalyzed command: ${cmdBase}` };
  }
  return evaluateTokens(tokens, j);
}

function checkRm(tokens, rmIndex) {
  const argv = tokens.map((t) => t.value);
  const flags = new Set();
  let sawDoubleDash = false;
  for (let i = rmIndex + 1; i < argv.length; i++) {
    const arg = argv[i];
    if (!sawDoubleDash && arg === "--") {
      sawDoubleDash = true;
      continue;
    }
    if (!sawDoubleDash && arg.startsWith("-")) {
      if (arg.startsWith("--")) {
        const name = arg.slice(2);
        if (["recursive", "r", "remove", "R"].includes(name)) flags.add("r");
        if (["force", "f"].includes(name)) flags.add("f");
      } else {
        for (const ch of arg.slice(1)) {
          if (ch === "r" || ch === "R") flags.add("r");
          if (ch === "f") flags.add("f");
        }
      }
      continue;
    }
    if ((flags.has("r") || flags.has("R")) && flags.has("f") && isDangerousRmTarget(arg)) {
      return { allowed: false, reason: "Blocked rm -rf on a dangerous target" };
    }
  }
  return { allowed: true };
}

function isDangerousRmTarget(arg) {
  let target = arg.replace(/^["']|["']$/g, "");

  // Reject any path component that is "..".
  if (/(^|[\/\\])\.\.($|[\/\\])/.test(target)) {
    return true;
  }

  // Hidden-file glob matches . and .. on most shells.
  if (/^\.\*/.test(target)) {
    return true;
  }

  const home = process.env.HOME || process.env.USERPROFILE || "";

  // Expand leading ~/ and ~.
  if (target === "~" || target === "~/" || target.startsWith("~/")) {
    target = target.replace(/^~/, home);
  } else if (/^~[^\/\\]/.test(target)) {
    // ~user is not resolvable without getpwnam; be safe.
    return true;
  }

  // Expand $HOME. Other variable references are rejected below.
  target = target.replace(/\$HOME/g, home);

  // Remaining unresolved variable references could expand to anything.
  if (target.includes("$")) {
    return true;
  }

  let normalized = path.normalize(target);
  if (normalized.endsWith(path.sep) && normalized.length > 1) {
    normalized = normalized.slice(0, -1);
  }

  // Empty path means the current directory.
  if (!normalized || normalized === ".") {
    return true;
  }

  // Root directories.
  if (normalized === path.sep || normalized === "/" || normalized === "\\") {
    return true;
  }
  if (/^[A-Za-z]:[\\]?$/.test(normalized)) {
    return true;
  }

  // The user's home directory.
  if (home && normalized.toLowerCase() === home.toLowerCase()) {
    return true;
  }

  // Windows system directory and anything inside it (C:\Windows, C:\Windows\System32, etc.).
  const systemRoot = (process.env.SystemRoot || process.env.windir || "").trim();
  if (systemRoot) {
    const lowerRoot = systemRoot.toLowerCase();
    const lowerNorm = normalized.toLowerCase();
    if (lowerNorm === lowerRoot || lowerNorm.startsWith(lowerRoot + "\\")) {
      return true;
    }
  }

  // Top-level absolute directories (e.g. /tmp, /etc, /var, C:\Temp, C:\Windows).
  // On Windows the drive letter counts as a segment, so allow one level under it.
  // Wildcard paths are handled separately below.
  if (!target.includes("*") && path.isAbsolute(normalized)) {
    const segments = normalized.split(path.sep).filter(Boolean);
    if (segments.length <= 2) {
      return true;
    }
  }

  // Paths that still contain unresolved ".." segments went above the root/CWD.
  if (normalized.split(path.sep).includes("..")) {
    return true;
  }

  // Wildcards directly under root, home, a Windows drive root, or a top-level directory.
  if (target.includes("*")) {
    const dirPart = target.split("*")[0];
    const normDir = path.normalize(dirPart || ".");
    if (
      normDir === path.sep ||
      normDir === "/" ||
      normDir === "\\" ||
      /^[A-Za-z]:[\\]?$/.test(normDir)
    ) {
      return true;
    }
    const cleanDir = normDir.replace(/[\/\\]+$/, "");
    if (home && cleanDir.toLowerCase() === home.toLowerCase()) {
      return true;
    }
    const endsWithSep = /[\/\\]$/.test(dirPart);
    if (!endsWithSep && path.isAbsolute(normDir)) {
      const dirSegments = normDir.split(path.sep).filter(Boolean);
      if (dirSegments.length <= 2) {
        return true;
      }
    }
  }

  return false;
}

function checkFind(tokens, i) {
  const argv = tokens.slice(i).map((t) => t.value);
  const dangerous = new Set(["-exec", "-execdir", "-ok", "-okdir", "-delete"]);
  for (const arg of argv) {
    if (dangerous.has(arg)) {
      return { allowed: false, reason: "Blocked dangerous find action" };
    }
  }
  return { allowed: true };
}
