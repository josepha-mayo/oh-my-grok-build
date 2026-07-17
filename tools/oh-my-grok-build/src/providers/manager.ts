import { writeFile, chmod, mkdir } from "node:fs/promises";
import { join } from "node:path";
import { homedir } from "node:os";
import type { ProviderConfig } from "../types.js";
import { loadOmgConfig, saveOmgConfig, syncProviderToGrokConfig } from "../config.js";

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
  return id.toLowerCase().replace(/[^a-z0-9_-]/g, "-").replace(/-+/g, "-").replace(/^-|-$/g, "");
}

export async function addProvider(input: ProviderInput): Promise<ProviderConfig> {
  const id = sanitizeId(input.id);
  if (!id) throw new Error("Provider id is required");
  const envKey = input.apiKey
    ? `OMGB_${id.replace(/-/g, "_").toUpperCase()}_API_KEY`
    : input.envKey;

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
}

export async function setDefaultProvider(id: string): Promise<void> {
  const cfg = await loadOmgConfig();
  if (!cfg.providers?.[id]) throw new Error(`Provider '${id}' not found`);
  cfg.defaultModel = `omgb-${id}`;
  await saveOmgConfig(cfg);
}

export async function writeApiKeyToEnv(providerId: string, key: string): Promise<void> {
  const omgDir = join(homedir(), ".omgb");
  await mkdir(omgDir, { recursive: true });
  const envPath = join(omgDir, ".env");
  const varName = `OMGB_${providerId.replace(/-/g, "_").toUpperCase()}_API_KEY`;
  const line = `${varName}=${key}\n`;
  // For simplicity we append/overwrite the single key line.
  const { readFile } = await import("node:fs/promises");
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
}
