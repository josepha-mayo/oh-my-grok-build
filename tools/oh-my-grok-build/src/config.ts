import { mkdir, readFile, writeFile, access, constants } from "node:fs/promises";
import { join } from "node:path";
import { homedir } from "node:os";
import { parse, stringify } from "smol-toml";
import type { OmgConfig, ProviderConfig } from "./types.js";

export function getGrokHome(): string {
  return process.env.GROK_HOME ?? join(homedir(), ".grok");
}

export function getGrokConfigPath(): string {
  return join(getGrokHome(), "config.toml");
}

export function getOmgDir(): string {
  return process.env.OMGB_HOME ? join(process.env.OMGB_HOME) : join(homedir(), ".omgb");
}

export function getOmgConfigPath(): string {
  return join(getOmgDir(), "config.json");
}

export async function ensureOmgDir(): Promise<void> {
  await mkdir(getOmgDir(), { recursive: true });
}

export async function loadOmgConfig(): Promise<OmgConfig> {
  const path = getOmgConfigPath();
  await ensureOmgDir();
  try {
    const raw = await readFile(path, "utf8");
    return JSON.parse(raw) as OmgConfig;
  } catch {
    return { providers: {} };
  }
}

export async function saveOmgConfig(config: OmgConfig): Promise<void> {
  await ensureOmgDir();
  await writeFile(getOmgConfigPath(), JSON.stringify(config, null, 2));
}

export async function loadGrokConfig(): Promise<Record<string, unknown>> {
  const path = getGrokConfigPath();
  try {
    await access(path, constants.R_OK);
  } catch {
    return {};
  }
  const raw = await readFile(path, "utf8");
  try {
    return parse(raw) as Record<string, unknown>;
  } catch (err) {
    throw new Error(`Failed to parse ${path}: ${err instanceof Error ? err.message : String(err)}`);
  }
}

export async function saveGrokConfig(config: Record<string, unknown>): Promise<void> {
  const path = getGrokConfigPath();
  await mkdir(getGrokHome(), { recursive: true });
  await writeFile(path, stringify(config));
}

/**
 * Write a provider into Grok's config as `[model.omgb-<id>]`.
 * NOTE: this round-trips through smol-toml, so comments in the file are not preserved.
 */
export async function syncProviderToGrokConfig(provider: ProviderConfig): Promise<void> {
  const config = await loadGrokConfig();
  const section = providerSection(provider);
  const models = (config["model"] as Record<string, unknown> | undefined) ?? {};
  models[`omgb-${provider.id}`] = section;
  config["model"] = models;

  if (config.models && typeof config.models === "object" && !Array.isArray(config.models)) {
    const modelsTable = config.models as Record<string, unknown>;
    if (!modelsTable.default) {
      modelsTable.default = `omgb-${provider.id}`;
    }
  } else {
    config.models = { default: `omgb-${provider.id}` };
  }

  await saveGrokConfig(config);
}

export function providerSection(provider: ProviderConfig): Record<string, unknown> {
  const section: Record<string, unknown> = {
    model: provider.model,
    base_url: provider.baseUrl,
    name: provider.name,
  };
  if (provider.apiBackend) section.api_backend = provider.apiBackend;
  if (provider.envKey) {
    section.env_key = Array.isArray(provider.envKey) ? provider.envKey[0] : provider.envKey;
  }
  if (provider.extraHeaders) section.extra_headers = provider.extraHeaders;
  if (provider.contextWindow) section.context_window = provider.contextWindow;
  if (provider.temperature !== undefined) section.temperature = provider.temperature;
  if (provider.topP !== undefined) section.top_p = provider.topP;
  if (provider.maxCompletionTokens) section.max_completion_tokens = provider.maxCompletionTokens;
  return section;
}

export async function loadOmgProviders(): Promise<Record<string, ProviderConfig>> {
  const cfg = await loadOmgConfig();
  return cfg.providers ?? {};
}

export async function saveOmgProvider(provider: ProviderConfig): Promise<void> {
  const cfg = await loadOmgConfig();
  cfg.providers = cfg.providers ?? {};
  cfg.providers[provider.id] = provider;
  await saveOmgConfig(cfg);
}

export async function removeOmgProvider(id: string): Promise<void> {
  const cfg = await loadOmgConfig();
  if (cfg.providers) {
    delete cfg.providers[id];
    await saveOmgConfig(cfg);
  }
}

export async function removeProviderFromGrokConfig(id: string): Promise<void> {
  const config = await loadGrokConfig();
  if (config["model"] && typeof config["model"] === "object" && !Array.isArray(config["model"])) {
    delete (config["model"] as Record<string, unknown>)[`omgb-${id}`];
  }
  if (
    config["models"] &&
    typeof config["models"] === "object" &&
    !Array.isArray(config["models"]) &&
    (config["models"] as Record<string, unknown>).default === `omgb-${id}`
  ) {
    const modelsTable = config["models"] as Record<string, unknown>;
    const remaining = Object.keys((config["model"] as Record<string, unknown>) ?? {});
    if (remaining.length > 0) {
      modelsTable.default = remaining[0];
    } else {
      delete modelsTable.default;
    }
  }
  await saveGrokConfig(config);
}

/**
 * Load `~/.omgb/.env` into `process.env` so spawned `grok` children can resolve
 * provider `env_key` values. This is intentionally a one-time load at startup;
 * it does not watch the file for changes.
 */
export async function loadOmgDotEnvIntoProcess(): Promise<void> {
  const envPath = join(getOmgDir(), ".env");
  let content = "";
  try {
    content = await readFile(envPath, "utf8");
  } catch {
    return;
  }
  for (const line of content.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const idx = trimmed.indexOf("=");
    if (idx === -1) continue;
    const key = trimmed.slice(0, idx).trim();
    const value = unquote(trimmed.slice(idx + 1).trim());
    if (key && process.env[key] === undefined) {
      process.env[key] = value;
    }
  }
}

export async function loadOmgDotEnv(): Promise<Record<string, string>> {
  const envPath = join(getOmgDir(), ".env");
  try {
    const content = await readFile(envPath, "utf8");
    const env: Record<string, string> = {};
    for (const line of content.split("\n")) {
      const trimmed = line.trim();
      if (!trimmed || trimmed.startsWith("#")) continue;
      const idx = trimmed.indexOf("=");
      if (idx === -1) continue;
      const key = trimmed.slice(0, idx).trim();
      const value = unquote(trimmed.slice(idx + 1).trim());
      if (key) env[key] = value;
    }
    return env;
  } catch {
    return {};
  }
}

function unquote(value: string): string {
  if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) {
    return value.slice(1, -1);
  }
  return value;
}
