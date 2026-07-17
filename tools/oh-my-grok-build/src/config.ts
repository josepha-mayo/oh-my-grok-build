import { mkdir, readFile, writeFile, access, constants } from "node:fs/promises";
import { join } from "node:path";
import { homedir } from "node:os";
import { parse, stringify } from "smol-toml";
import type { OmgConfig, ProviderConfig } from "./types.js";

const OMG_DIR = join(homedir(), ".omgb");
const OMG_CONFIG = join(OMG_DIR, "config.json");

export function getGrokHome(): string {
  return process.env.GROK_HOME ?? join(homedir(), ".grok");
}

export function getGrokConfigPath(): string {
  return join(getGrokHome(), "config.toml");
}

export function getOmgDir(): string {
  return OMG_DIR;
}

export async function ensureOmgDir(): Promise<void> {
  await mkdir(OMG_DIR, { recursive: true });
}

export async function loadOmgConfig(): Promise<OmgConfig> {
  await ensureOmgDir();
  try {
    const raw = await readFile(OMG_CONFIG, "utf8");
    return JSON.parse(raw) as OmgConfig;
  } catch {
    return { providers: {} };
  }
}

export async function saveOmgConfig(config: OmgConfig): Promise<void> {
  await ensureOmgDir();
  await writeFile(OMG_CONFIG, JSON.stringify(config, null, 2));
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
  if (provider.apiKey) section.api_key = provider.apiKey;
  if (provider.envKey) section.env_key = provider.envKey;
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
