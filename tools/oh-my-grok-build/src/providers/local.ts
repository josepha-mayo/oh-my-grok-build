import type { ProviderConfig } from "../types.js";
import { loadOmgDotEnv } from "../config.js";

const OLLAMA_DEFAULT = "http://localhost:11434/v1";
const LMSTUDIO_DEFAULT = "http://localhost:1234/v1";

export async function probeOllama(baseUrl: string = OLLAMA_DEFAULT): Promise<string[]> {
  return listModelsAt(baseUrl);
}

export async function probeLmStudio(baseUrl: string = LMSTUDIO_DEFAULT): Promise<string[]> {
  return listModelsAt(baseUrl);
}

async function listModelsAt(baseUrl: string): Promise<string[]> {
  const url = `${baseUrl.replace(/\/+$/, "")}/models`;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 5000);
  try {
    const res = await fetch(url, { signal: controller.signal });
    if (!res.ok) return [];
    const json = (await res.json()) as { data?: { id: string }[] };
    if (!Array.isArray(json?.data)) return [];
    return json.data.map((m) => m.id);
  } catch {
    return [];
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
    const res = await fetch(`${baseUrl}/models`, { headers, signal: controller.signal });
    if (!res.ok) return false;
    const json = (await res.json()) as { data?: unknown };
    return Array.isArray(json?.data);
  } catch {
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
    const res = await fetch(`${baseUrl}/chat/completions`, {
      method: "POST",
      headers: { "Content-Type": "application/json", ...headers },
      signal: controller.signal,
      body: JSON.stringify({
        model: provider.model,
        messages: [{ role: "system", content: "ping" }],
        max_tokens: 1,
      }),
    });
    if (res.ok) return { ok: true };
    const text = await res.text();
    return { ok: false, error: `${res.status}: ${text.slice(0, 200)}` };
  } catch (err) {
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
    const res = await fetch(`${baseUrl}/responses`, {
      method: "POST",
      headers: { "Content-Type": "application/json", ...headers },
      signal: controller.signal,
      body: JSON.stringify({
        model: provider.model,
        input: "ping",
        max_output_tokens: 1,
      }),
    });
    if (res.ok) return { ok: true };
    const text = await res.text();
    return { ok: false, error: `${res.status}: ${text.slice(0, 200)}` };
  } catch (err) {
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
    const res = await fetch(`${baseUrl}/messages`, {
      method: "POST",
      headers: { "Content-Type": "application/json", ...headers },
      signal: controller.signal,
      body: JSON.stringify({
        model: provider.model,
        max_tokens: 1,
        messages: [{ role: "user", content: "ping" }],
      }),
    });
    if (res.ok) return { ok: true };
    const text = await res.text();
    return { ok: false, error: `${res.status}: ${text.slice(0, 200)}` };
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : String(err) };
  } finally {
    clearTimeout(timeout);
  }
}
