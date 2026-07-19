import chalk from "chalk";
import { loadOmgConfig, saveOmgConfig } from "../config.js";
import { sanitizeUserEnv } from "../env.js";
import { isBuiltinMcpServer, loadMcpConfig, toAcpMcpServers, validateMcpServerConfig } from "../mcp/mcp-config.js";
import type { McpServerConfig } from "../types.js";

export async function toolsListCommand(): Promise<void> {
  const servers = await loadMcpConfig();
  console.log(chalk.bold("\nConfigured MCP tools:\n"));
  for (const s of servers) {
    const status = s.enabled ? chalk.green("enabled") : chalk.gray("disabled");
    console.log(`  ${chalk.cyan(s.name)}  ${status}`);
    console.log(`    command: ${s.command} ${s.args.join(" ")}`);
  }
  const active = toAcpMcpServers(servers);
  console.log(chalk.bold(`\nActive at next session: ${active.length} server(s)`));
}

const DANGEROUS_SERVERS: Record<string, string> = {
  "omgb-browser": "omgb-browser lets the agent navigate arbitrary public websites and interact with pages.",
  "omgb-computer": "omgb-computer lets the agent control your real desktop (mouse, keyboard, screenshots).",
};

async function updateBuiltinState(name: string, enabled: boolean): Promise<void> {
  if (!isBuiltinMcpServer(name)) {
    throw new Error(`'${name}' is not a built-in MCP server`);
  }
  const cfg = await loadOmgConfig();
  const servers = cfg.mcpServers ?? [];
  const existing = servers.find((s) => s.name === name);
  if (existing) {
    existing.enabled = enabled;
  } else {
    servers.push({ name, enabled, command: "node", args: [] });
  }
  cfg.mcpServers = servers;
  await saveOmgConfig(cfg);
}

export async function toolsEnableCommand(name: string): Promise<void> {
  if (name === "omgb-computer" && process.env.OMGB_ALLOW_DESKTOP_CONTROL !== "1") {
    throw new Error(
      "omgb-computer controls your real desktop and is disabled by default. " +
        "Set OMGB_ALLOW_DESKTOP_CONTROL=1 in your environment (e.g. in ~/.omgb/.env) and re-run this command."
    );
  }
  await updateBuiltinState(name, true);
  const warning = DANGEROUS_SERVERS[name];
  if (warning) console.log(chalk.yellow(`Warning: ${warning}`));
  console.log(chalk.green(`Enabled '${name}'.`));
}

export async function toolsDisableCommand(name: string): Promise<void> {
  await updateBuiltinState(name, false);
  console.log(chalk.green(`Disabled '${name}'.`));
}

export async function toolsAddCommand(
  name: string,
  command: string,
  args: string[],
  options: { env?: string[] }
): Promise<void> {
  if (isBuiltinMcpServer(name)) {
    throw new Error(`'${name}' is a built-in server; use 'omgb tools enable ${name}' to enable it.`);
  }

  const rawEnv: Record<string, string> = {};
  for (const e of options.env ?? []) {
    const idx = e.indexOf("=");
    if (idx === -1) throw new Error(`Invalid env var: ${e} (expected NAME=VALUE)`);
    rawEnv[e.slice(0, idx)] = e.slice(idx + 1);
  }
  const env = sanitizeUserEnv(rawEnv);
  const dropped = Object.keys(rawEnv).filter((k) => !(k in env));
  if (dropped.length) {
    console.warn(chalk.yellow(`Ignored non-API-key env variables for safety: ${dropped.join(", ")}`));
  }

  const cfg = await loadOmgConfig();
  const servers = cfg.mcpServers ?? [];
  const updated: McpServerConfig = { name, enabled: true, command, args, env };
  validateMcpServerConfig(updated);

  const existingIndex = servers.findIndex((s) => s.name === name);
  if (existingIndex >= 0) {
    servers[existingIndex] = updated;
  } else {
    servers.push(updated);
  }
  cfg.mcpServers = servers;
  await saveOmgConfig(cfg);
  console.log(chalk.green(`Saved MCP server '${name}'.`));
}

export async function toolsRemoveCommand(name: string): Promise<void> {
  if (isBuiltinMcpServer(name)) {
    throw new Error(`'${name}' is a built-in server; use 'omgb tools disable ${name}' to disable it.`);
  }
  const cfg = await loadOmgConfig();
  cfg.mcpServers = (cfg.mcpServers ?? []).filter((s) => s.name !== name);
  await saveOmgConfig(cfg);
  console.log(chalk.green(`Removed '${name}'.`));
}
