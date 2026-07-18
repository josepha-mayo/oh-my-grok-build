import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { getOmgDir, atomicWriteFile } from "../config.js";
import { ClaudeConnector } from "./claude.js";
import { CodexConnector } from "./codex.js";
import { OpenCodeConnector } from "./opencode.js";
import type { Connector, ConnectorConfig, ConnectorRegistry } from "./types.js";

const REGISTRY_PATH = () => join(getOmgDir(), "connectors.json");

const RESERVED_CONNECTOR_NAMES = new Set(["__proto__", "prototype", "constructor"]);

function createNullRegistry(): ConnectorRegistry {
  return { connectors: Object.create(null) as Record<string, ConnectorConfig> };
}

export async function loadRegistry(): Promise<ConnectorRegistry> {
  try {
    const raw = await readFile(REGISTRY_PATH(), "utf8");
    const parsed = JSON.parse(raw) as ConnectorRegistry;
    const registry = createNullRegistry();
    if (parsed && typeof parsed === "object" && parsed.connectors && typeof parsed.connectors === "object") {
      for (const [k, v] of Object.entries(parsed.connectors)) {
        (registry.connectors as Record<string, ConnectorConfig>)[k] = v;
      }
    }
    return registry;
  } catch {
    return createNullRegistry();
  }
}

export async function saveRegistry(registry: ConnectorRegistry): Promise<void> {
  await atomicWriteFile(REGISTRY_PATH(), JSON.stringify(registry, null, 2));
}

function validateName(name: string): void {
  if (RESERVED_CONNECTOR_NAMES.has(name)) {
    throw new Error(`Reserved connector name: ${name}`);
  }
  if (!/^[a-zA-Z0-9_-]{1,64}$/.test(name)) {
    throw new Error(`Invalid connector name: ${name}`);
  }
}

export async function listConnectors(): Promise<ConnectorConfig[]> {
  const registry = await loadRegistry();
  return Object.values(registry.connectors);
}

export async function getConnector(name: string): Promise<ConnectorConfig | undefined> {
  validateName(name);
  const registry = await loadRegistry();
  return registry.connectors[name];
}

export async function addConnector(config: ConnectorConfig): Promise<ConnectorConfig> {
  validateName(config.name);
  const registry = await loadRegistry();
  registry.connectors[config.name] = config;
  await saveRegistry(registry);
  return config;
}

export async function removeConnector(name: string): Promise<void> {
  validateName(name);
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
