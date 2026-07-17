import { writeFile, chmod, mkdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import type { ProviderConfig } from "../types.js";
import {
  loadOmgConfig,
  saveOmgConfig,
  loadGrokConfig,
  saveGrokConfig,
  syncProviderToGrokConfig,
  removeProviderFromGrokConfig,
  getOmgDir,
} from "../config.js";

export type ProviderInput = {
  id: string;
  name?: string;
  model: string;
  baseUrl: string;
  apiBackend?: ProviderConfig["apiBackend"];
  apiKey?: string;
  envKey?: string | string[];
  extraHeaders?: Record<string, string>;
  contextWindow?: number;
  temperature?: number;
  topP?: number;
  maxCompletionTokens?: number;
};

function sanitizeId(id: string): string {
  return id
    .toLowerCase()
    .replace(/[^a-z0-9_-]/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
}

function envVarName(providerId: string): string {
  return `OMGB_${providerId.replace(/-/g, "_").toUpperCase()}_API_KEY`;
}

export async function addProvider(input: ProviderInput): Promise<ProviderConfig> {
  const id = sanitizeId(input.id);
  if (!id) throw new Error("Provider id is required");
  const envKey = input.apiKey ? envVarName(id) : input.envKey;

  const config: ProviderConfig = {
    id,
    name: input.name ?? input.id,
    model: input.model,
    baseUrl: input.baseUrl,
    apiBackend: input.apiBackend ?? "chat_completions",
    envKey: envKey ? (Array.isArray(envKey) ? envKey : [envKey]) : undefined,
    extraHeaders: input.extraHeaders,
    contextWindow: input.contextWindow,
    temperature: input.temperature,
    topP: input.topP,
    maxCompletionTokens: input.maxCompletionTokens,
  };

  if (input.apiKey) {
    await writeApiKeyToEnv(id, input.apiKey);
  }

  const cfg = await loadOmgConfig();
  cfg.providers = cfg.providers ?? {};
  cfg.providers[id] = config;
  if (!cfg.defaultModel) cfg.defaultModel = `omgb-${id}`;
  await saveOmgConfig(cfg);

  // Sync into Grok's config so `grok` can resolve the model.
  await syncProviderToGrokConfig(config);

  return config;
}

export async function listProviders(): Promise<ProviderConfig[]> {
  const cfg = await loadOmgConfig();
  return Object.values(cfg.providers ?? {});
}

export async function getProvider(id: string): Promise<ProviderConfig | undefined> {
  const cfg = await loadOmgConfig();
  return cfg.providers?.[id];
}

export async function removeProvider(id: string): Promise<void> {
  const cfg = await loadOmgConfig();
  if (cfg.providers?.[id]) {
    delete cfg.providers[id];
    await saveOmgConfig(cfg);
  }
  await removeApiKeyFromEnv(id);
  await removeProviderFromGrokConfig(id);
}

export async function setDefaultProvider(id: string): Promise<void> {
  const cfg = await loadOmgConfig();
  if (!cfg.providers?.[id]) throw new Error(`Provider '${id}' not found`);
  const modelId = `omgb-${id}`;
  cfg.defaultModel = modelId;
  await saveOmgConfig(cfg);

  const gcfg = await loadGrokConfig();
  if (!gcfg.models || typeof gcfg.models !== "object" || Array.isArray(gcfg.models)) {
    gcfg.models = {};
  }
  (gcfg.models as Record<string, unknown>).default = modelId;
  await saveGrokConfig(gcfg);
}

export async function writeApiKeyToEnv(providerId: string, key: string): Promise<void> {
  const omgDir = getOmgDir();
  await mkdir(omgDir, { recursive: true });
  const envPath = join(omgDir, ".env");
  const varName = envVarName(providerId);
  const line = `${varName}=${key}\n`;

  let content = "";
  try {
    content = await readFile(envPath, "utf8");
  } catch {
    // file does not exist
  }
  const lines = content.split("\n").filter((l) => !l.startsWith(`${varName}=`));
  lines.push(line);
  await writeFile(envPath, lines.join("\n") + (lines[lines.length - 1]?.endsWith("\n") ? "" : "\n"));
  if (process.platform !== "win32") {
    await chmod(envPath, 0o600);
  }

  // Make the key available to spawned grok processes immediately.
  process.env[varName] = key;
}

export async function removeApiKeyFromEnv(providerId: string): Promise<void> {
  const omgDir = getOmgDir();
  const envPath = join(omgDir, ".env");
  const varName = envVarName(providerId);

  let content = "";
  try {
    content = await readFile(envPath, "utf8");
  } catch {
    return;
  }
  const lines = content.split("\n").filter((l) => !l.startsWith(`${varName}=`));
  const newContent = lines.join("\n").trimEnd();
  if (newContent) {
    await writeFile(envPath, newContent + "\n");
  } else {
    await writeFile(envPath, "");
  }

  delete process.env[varName];
}
