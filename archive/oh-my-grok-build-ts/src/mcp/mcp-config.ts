import { fileURLToPath } from "node:url";
import { dirname, isAbsolute, join } from "node:path";
import { existsSync } from "node:fs";
import { loadOmgConfig } from "../config.js";
import { sanitizeUserEnv } from "../env.js";
import type { McpServerConfig } from "../types.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

export interface AcpMcpServerEntry {
  type: "stdio";
  name: string;
  command: string;
  args: string[];
  env?: { name: string; value: string }[];
  [key: string]: unknown;
}

const MCP_NAME_RE = /^[a-zA-Z0-9_-]{1,64}$/;
const SHELL_METACHARS = /[;|&<>(){}$`!*?"\n\r]/;
const SAFE_INTERPRETERS = new Set(["node", "node.exe", "python", "python3", "python.exe", "python3.exe"]);
const DANGEROUS_BASENAMES = new Set([
  "sh",
  "bash",
  "zsh",
  "fish",
  "csh",
  "ksh",
  "dash",
  "cmd",
  "cmd.exe",
  "powershell",
  "powershell.exe",
  "pwsh",
  "pwsh.exe",
  "command",
  "command.com",
  "explorer.exe",
  "start",
]);

function findScript(name: string): string {
  const candidate = join(__dirname, `${name}.js`);
  if (existsSync(candidate)) return candidate;
  throw new Error(`Built-in MCP script not found: ${name}`);
}

function basename(command: string): string {
  const idx = Math.max(command.lastIndexOf("/"), command.lastIndexOf("\\"));
  return idx >= 0 ? command.slice(idx + 1) : command;
}

export function isBuiltinMcpServer(name: string): boolean {
  return builtInMcpServers().some((s) => s.name === name);
}

export function builtInMcpServer(name: string): McpServerConfig | undefined {
  return builtInMcpServers().find((s) => s.name === name);
}

export function builtInMcpServers(): McpServerConfig[] {
  return [
    { name: "omgb-memory", enabled: true, command: "node", args: [findScript("memory")] },
    { name: "omgb-browser", enabled: false, command: "node", args: [findScript("browser")] },
    { name: "omgb-computer", enabled: false, command: "node", args: [findScript("computer")] },
  ];
}

export function validateMcpServerConfig(config: McpServerConfig): void {
  if (!MCP_NAME_RE.test(config.name)) {
    throw new Error(`Invalid MCP server name: ${config.name}`);
  }
  if (isBuiltinMcpServer(config.name)) {
    throw new Error(`'${config.name}' is a built-in server; use 'omgb tools enable/disable' to manage it.`);
  }
  if (!config.command || SHELL_METACHARS.test(config.command)) {
    throw new Error(`MCP command is empty or contains shell metacharacters: ${config.command}`);
  }
  const base = basename(config.command).toLowerCase();
  if (DANGEROUS_BASENAMES.has(base)) {
    throw new Error(`Dangerous MCP command not allowed: ${base}`);
  }
  const absolute = isAbsolute(config.command);
  const isInterpreter =
    SAFE_INTERPRETERS.has(config.command.toLowerCase()) || (absolute && SAFE_INTERPRETERS.has(base));
  if (!isInterpreter && !absolute) {
    throw new Error(`MCP command must be an absolute path or one of: ${[...SAFE_INTERPRETERS].join(", ")}`);
  }
  if (!Array.isArray(config.args) || config.args.some((a) => typeof a !== "string")) {
    throw new Error("MCP args must be an array of strings");
  }
  for (const arg of config.args) {
    if (SHELL_METACHARS.test(arg)) {
      throw new Error(`MCP arg contains shell metacharacters: ${arg}`);
    }
  }
  if (isInterpreter) {
    const script = config.args[0];
    if (!script || script.startsWith("-")) {
      throw new Error(`MCP interpreter '${config.command}' requires a script path as the first argument`);
    }
    if (!isAbsolute(script)) {
      throw new Error(`MCP interpreter '${config.command}' script must be an absolute path: ${script}`);
    }
  }
}

export function mergeMcpConfigs(stored: McpServerConfig[] | undefined): McpServerConfig[] {
  const builtins = builtInMcpServers();
  const result = new Map<string, McpServerConfig>();
  for (const s of builtins) {
    result.set(s.name, { ...s });
  }
  for (const s of stored ?? []) {
    const builtin = result.get(s.name);
    if (builtin) {
      // Built-ins can only be enabled/disabled from config; command/args/env are fixed.
      result.set(s.name, { ...builtin, enabled: s.enabled });
    } else {
      validateMcpServerConfig(s);
      result.set(s.name, { ...s });
    }
  }
  return Array.from(result.values());
}

export async function loadMcpConfig(): Promise<McpServerConfig[]> {
  const cfg = await loadOmgConfig();
  return mergeMcpConfigs(cfg.mcpServers);
}

export function toAcpMcpServers(servers: McpServerConfig[]): AcpMcpServerEntry[] {
  return servers
    .filter((s) => s.enabled)
    .map((s) => {
      const entry: AcpMcpServerEntry = { type: "stdio", name: s.name, command: s.command, args: s.args };
      const safeEnv = sanitizeUserEnv(s.env);
      if (Object.keys(safeEnv).length) {
        entry.env = Object.entries(safeEnv).map(([name, value]) => ({ name, value }));
      }
      return entry;
    });
}
