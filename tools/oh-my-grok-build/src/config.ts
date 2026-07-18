import { mkdir, readFile, writeFile, access, constants, rename } from "node:fs/promises";
import { join, dirname } from "node:path";
import { homedir } from "node:os";
import { parse, stringify } from "smol-toml";
import type { OmgConfig, ProviderConfig } from "./types.js";

export const DEFAULT_MODEL = "grok-4.5";

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

export async function atomicWriteFile(path: string, content: string): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  const tmp = `${path}.tmp.${Date.now()}`;
  try {
    await writeFile(tmp, content, { mode: 0o600 });
    await rename(tmp, path);
  } catch {
    // Fallback for platforms where cross-device rename fails.
    await writeFile(path, content, { mode: 0o600 });
  }
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
  await atomicWriteFile(getOmgConfigPath(), JSON.stringify(config, null, 2));
}

export function applyGrokDefaults(config: Record<string, unknown>): Record<string, unknown> {
  const cfg = config;
  if (!cfg.features || typeof cfg.features !== "object" || Array.isArray(cfg.features)) {
    cfg.features = {};
  }
  const features = cfg.features as Record<string, unknown>;
  if (features.telemetry === undefined) {
    features.telemetry = false;
  }

  if (!cfg.telemetry || typeof cfg.telemetry !== "object" || Array.isArray(cfg.telemetry)) {
    cfg.telemetry = {};
  }
  const telemetry = cfg.telemetry as Record<string, unknown>;
  for (const [key, value] of [
    ["mixpanel_enabled", false],
    ["trace_upload", false],
    ["otel_enabled", false],
    ["otel_log_user_prompts", false],
    ["otel_log_tool_details", false],
  ] as const) {
    if (telemetry[key] === undefined) telemetry[key] = value;
  }

  return cfg;
}

export async function loadGrokConfig(): Promise<Record<string, unknown>> {
  const path = getGrokConfigPath();
  let rawConfig: Record<string, unknown> = {};
  try {
    await access(path, constants.R_OK);
    const raw = await readFile(path, "utf8");
    rawConfig = parse(raw) as Record<string, unknown>;
  } catch {
    // file does not exist yet
  }
  return applyGrokDefaults(rawConfig);
}

export async function saveGrokConfig(config: Record<string, unknown>): Promise<void> {
  const path = getGrokConfigPath();
  await mkdir(getGrokHome(), { recursive: true });
  await atomicWriteFile(path, stringify(applyGrokDefaults(config)));
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

/**
 * Write a provider into Grok's config as `[model.omgb-<id>]`.
 * The file is round-tripped through smol-toml, so comments are not preserved.
 * The file is written atomically and a `.bak` copy is kept.
 */
export async function syncProviderToGrokConfig(provider: ProviderConfig): Promise<void> {
  const path = getGrokConfigPath();
  await mkdir(getGrokHome(), { recursive: true });

  let config = await loadGrokConfig();
  try {
    const raw = await readFile(path, "utf8");
    await atomicWriteFile(`${path}.bak`, raw);
  } catch {
    // file does not exist yet
  }

  const modelKey = `omgb-${provider.id}`;
  const modelTable = (config["model"] as Record<string, unknown> | undefined) ?? {};
  modelTable[modelKey] = providerSection(provider);
  config["model"] = modelTable;

  const modelsTable = (config["models"] as Record<string, unknown> | undefined) ?? {};
  if (!modelsTable.default) {
    modelsTable.default = modelKey;
    config["models"] = modelsTable;
  }

  await saveGrokConfig(config);
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
  const path = getGrokConfigPath();
  let config = await loadGrokConfig();
  try {
    const raw = await readFile(path, "utf8");
    await atomicWriteFile(`${path}.bak`, raw);
  } catch {
    return;
  }

  const modelKey = `omgb-${id}`;
  const modelTable = (config["model"] as Record<string, unknown> | undefined) ?? {};
  delete modelTable[modelKey];
  config["model"] = modelTable;

  const modelsTable = (config["models"] as Record<string, unknown> | undefined) ?? {};
  if (modelsTable.default === modelKey) {
    const remaining = Object.keys(modelTable);
    if (remaining.length > 0) {
      modelsTable.default = remaining[0];
    } else {
      delete modelsTable.default;
    }
    config["models"] = modelsTable;
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
    // Only load API-key-like variables; never set sensitive process env vars
    // such as PATH, LD_PRELOAD, or SHELL from a user-editable dotenv file.
    if (key && process.env[key] === undefined && /^[_A-Z0-9]+_API_KEY$/i.test(key)) {
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
