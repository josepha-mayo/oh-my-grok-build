import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { existsSync } from "node:fs";
import { loadOmgConfig } from "../config.js";
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

function findScript(name: string): string | undefined {
  const candidates = [join(__dirname, `${name}.js`), join(__dirname, "..", "src", "mcp", `${name}.js`)];
  for (const c of candidates) {
    if (existsSync(c)) return c;
  }
  return undefined;
}

export function builtInMcpServers(): McpServerConfig[] {
  const script = (name: string) => findScript(name) ?? name;
  return [
    { name: "omgb-memory", enabled: true, command: "node", args: [script("memory")] },
    { name: "omgb-browser", enabled: false, command: "node", args: [script("browser")] },
    { name: "omgb-computer", enabled: false, command: "node", args: [script("computer")] },
  ];
}

export function mergeMcpConfigs(stored: McpServerConfig[] | undefined): McpServerConfig[] {
  const builtins = new Map(builtInMcpServers().map((s) => [s.name, s]));
  for (const s of stored ?? []) {
    const existing = builtins.get(s.name);
    if (existing) {
      builtins.set(s.name, {
        ...existing,
        enabled: s.enabled,
        command: s.command || existing.command,
        args: s.args?.length ? s.args : existing.args,
        env: s.env ?? existing.env,
      });
    } else {
      builtins.set(s.name, s);
    }
  }
  return Array.from(builtins.values());
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
      if (s.env && Object.keys(s.env).length) {
        entry.env = Object.entries(s.env).map(([name, value]) => ({ name, value }));
      }
      return entry;
    });
}
