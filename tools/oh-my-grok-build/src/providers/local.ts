import type { ProviderConfig } from "../types.js";
import { loadOmgDotEnv } from "../config.js";
import { formatProviderError } from "./errors.js";
import { resolveProviderUrl } from "../net.js";
import { fetch as undiciFetch, Agent } from "undici";

const OLLAMA_DEFAULT = "http://localhost:11434/v1";
const LMSTUDIO_DEFAULT = "http://localhost:1234/v1";

export class UrlValidationError extends Error {
  readonly validation = true;
}

export interface SafeFetchResult {
  status: number;
  headers: Headers;
  body: string;
}

async function safeFetch(
  inputUrl: string,
  init: { method?: string; headers?: Record<string, string>; body?: string; signal?: AbortSignal } = {}
): Promise<SafeFetchResult> {
  const result = await resolveProviderUrl(inputUrl);
  if (!result.ok) throw new UrlValidationError(result.reason);
  const { url, host, lookup } = result;
  const dispatcher = lookup ? new Agent({ connect: { servername: host, lookup } }) : undefined;
  try {
    const res = await undiciFetch(url.toString(), { ...init, dispatcher, redirect: "error" });
    const body = await res.text();
    return { status: res.status, headers: res.headers, body };
  } finally {
    await dispatcher?.close();
  }
}

export async function probeOllama(baseUrl: string = OLLAMA_DEFAULT): Promise<string[]> {
  return listModelsAt(baseUrl);
}

export async function probeLmStudio(baseUrl: string = LMSTUDIO_DEFAULT): Promise<string[]> {
  return listModelsAt(baseUrl);
}

async function listModelsAt(baseUrl: string): Promise<string[]> {
  return (await fetchModelList(baseUrl)) ?? [];
}

export async function fetchModelList(
  baseUrl: string,
  apiKey?: string,
  apiBackend: string = "chat_completions",
  extraHeaders: Record<string, string> = {}
): Promise<string[] | undefined> {
  const url = `${baseUrl.replace(/\/+$/, "")}/models`;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 10000);
  const headers: Record<string, string> = { ...extraHeaders };
  if (apiKey) {
    if (apiBackend === "messages") {
      headers["x-api-key"] = apiKey;
    } else {
      headers["Authorization"] = `Bearer ${apiKey}`;
    }
  }
  try {
    const res = await safeFetch(url, { headers, signal: controller.signal });
    if (res.status !== 200) return undefined;
    const json = JSON.parse(res.body) as { data?: { id: string }[] };
    if (!Array.isArray(json?.data)) return undefined;
    return json.data.map((m) => m.id);
  } catch {
    return undefined;
  } finally {
    clearTimeout(timeout);
  }
}

export async function discoverLocalModels(
  baseUrls: { ollama?: string; lmstudio?: string } = {}
): Promise<{ provider: "ollama" | "lmstudio"; models: string[] }[]> {
  const [ollama, lmstudio] = await Promise.all([probeOllama(baseUrls.ollama), probeLmStudio(baseUrls.lmstudio)]);
  const found: { provider: "ollama" | "lmstudio"; models: string[] }[] = [];
  if (ollama.length) found.push({ provider: "ollama", models: ollama });
  if (lmstudio.length) found.push({ provider: "lmstudio", models: lmstudio });
  return found;
}

export async function resolveApiKey(provider: ProviderConfig): Promise<string | undefined> {
  const keys = typeof provider.envKey === "string" ? [provider.envKey] : (provider.envKey ?? []);
  for (const k of keys) {
    const v = process.env[k];
    if (v) return v;
  }
  const dotenv = await loadOmgDotEnv();
  for (const k of keys) {
    if (dotenv[k]) return dotenv[k];
  }
  return undefined;
}

export async function testProvider(provider: ProviderConfig): Promise<{ ok: boolean; error?: string }> {
  const apiKey = await resolveApiKey(provider);
  const baseUrl = provider.baseUrl.replace(/\/+$/, "");
  const backend = provider.apiBackend ?? "chat_completions";
  const headers: Record<string, string> = {};
  if (apiKey) {
    if (backend === "messages") {
      headers["x-api-key"] = apiKey;
    } else {
      headers["Authorization"] = `Bearer ${apiKey}`;
    }
  }
  if (provider.extraHeaders) Object.assign(headers, provider.extraHeaders);

  try {
    const ok = await testModelsList(baseUrl, headers);
    if (ok) return { ok: true };

    if (backend === "chat_completions") {
      return testTinyChatCompletion(provider, baseUrl, headers);
    }
    if (backend === "responses") {
      return testTinyResponses(provider, baseUrl, headers);
    }
    if (backend === "messages") {
      return testTinyMessages(provider, baseUrl, headers);
    }

    return { ok: false, error: "Provider did not respond to the models list" };
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : String(err) };
  }
}

async function testModelsList(baseUrl: string, headers: Record<string, string>): Promise<boolean> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 10000);
  try {
    const res = await safeFetch(`${baseUrl}/models`, { headers, signal: controller.signal });
    if (res.status !== 200) return false;
    const json = JSON.parse(res.body) as { data?: unknown };
    return Array.isArray(json?.data);
  } catch (err) {
    if (err instanceof UrlValidationError) throw err;
    return false;
  } finally {
    clearTimeout(timeout);
  }
}

async function testTinyChatCompletion(
  provider: ProviderConfig,
  baseUrl: string,
  headers: Record<string, string>
): Promise<{ ok: boolean; error?: string }> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 10000);
  try {
    const res = await safeFetch(`${baseUrl}/chat/completions`, {
      method: "POST",
      headers: { "Content-Type": "application/json", ...headers },
      signal: controller.signal,
      body: JSON.stringify({
        model: provider.model,
        messages: [{ role: "system", content: "ping" }],
        max_tokens: 1,
      }),
    });
    if (res.status === 200) return { ok: true };
    return { ok: false, error: formatProviderError(res.status, res.body) };
  } catch (err) {
    if (err instanceof UrlValidationError) throw err;
    return { ok: false, error: err instanceof Error ? err.message : String(err) };
  } finally {
    clearTimeout(timeout);
  }
}

async function testTinyResponses(
  provider: ProviderConfig,
  baseUrl: string,
  headers: Record<string, string>
): Promise<{ ok: boolean; error?: string }> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 10000);
  try {
    const res = await safeFetch(`${baseUrl}/responses`, {
      method: "POST",
      headers: { "Content-Type": "application/json", ...headers },
      signal: controller.signal,
      body: JSON.stringify({
        model: provider.model,
        input: "ping",
        max_output_tokens: 1,
      }),
    });
    if (res.status === 200) return { ok: true };
    return { ok: false, error: formatProviderError(res.status, res.body) };
  } catch (err) {
    if (err instanceof UrlValidationError) throw err;
    return { ok: false, error: err instanceof Error ? err.message : String(err) };
  } finally {
    clearTimeout(timeout);
  }
}

async function testTinyMessages(
  provider: ProviderConfig,
  baseUrl: string,
  headers: Record<string, string>
): Promise<{ ok: boolean; error?: string }> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 10000);
  try {
    const res = await safeFetch(`${baseUrl}/messages`, {
      method: "POST",
      headers: { "Content-Type": "application/json", ...headers },
      signal: controller.signal,
      body: JSON.stringify({
        model: provider.model,
        max_tokens: 1,
        messages: [{ role: "user", content: "ping" }],
      }),
    });
    if (res.status === 200) return { ok: true };
    return { ok: false, error: formatProviderError(res.status, res.body) };
  } catch (err) {
    if (err instanceof UrlValidationError) throw err;
    return { ok: false, error: err instanceof Error ? err.message : String(err) };
  } finally {
    clearTimeout(timeout);
  }
}
