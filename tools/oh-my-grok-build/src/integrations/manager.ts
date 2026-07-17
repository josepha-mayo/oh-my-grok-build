import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { getOmgDir } from "../config.js";
import { ClaudeConnector } from "./claude.js";
import { CodexConnector } from "./codex.js";
import { OpenCodeConnector } from "./opencode.js";
import type { Connector, ConnectorConfig, ConnectorRegistry } from "./types.js";

const REGISTRY_PATH = () => join(getOmgDir(), "connectors.json");

export async function loadRegistry(): Promise<ConnectorRegistry> {
  try {
    const raw = await readFile(REGISTRY_PATH(), "utf8");
    return JSON.parse(raw) as ConnectorRegistry;
  } catch {
    return { connectors: {} };
  }
}

export async function saveRegistry(registry: ConnectorRegistry): Promise<void> {
  await writeFile(REGISTRY_PATH(), JSON.stringify(registry, null, 2));
}

export async function listConnectors(): Promise<ConnectorConfig[]> {
  const registry = await loadRegistry();
  return Object.values(registry.connectors);
}

export async function getConnector(name: string): Promise<ConnectorConfig | undefined> {
  const registry = await loadRegistry();
  return registry.connectors[name];
}

export async function addConnector(config: ConnectorConfig): Promise<ConnectorConfig> {
  const registry = await loadRegistry();
  registry.connectors[config.name] = config;
  await saveRegistry(registry);
  return config;
}

export async function removeConnector(name: string): Promise<void> {
  const registry = await loadRegistry();
  delete registry.connectors[name];
  await saveRegistry(registry);
}

export function buildConnector(config: ConnectorConfig): Connector {
  switch (config.type) {
    case "opencode":
      return new OpenCodeConnector(config);
    case "codex":
      return new CodexConnector(config);
    case "claude":
      return new ClaudeConnector(config);
    default:
      throw new Error(`Unknown connector type: ${config.type}`);
  }
}

export async function runConnector(name: string, prompt: string): Promise<string> {
  const cfg = await getConnector(name);
  if (!cfg) throw new Error(`Connector '${name}' not found`);
  const connector = buildConnector(cfg);
  const result = await connector.run(prompt);
  await connector.close?.();
  return result.text;
}
