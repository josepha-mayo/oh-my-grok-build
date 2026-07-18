import { persistGet, persistSet, secureGetJson, secureSetJson } from "./storage";

export interface Provider {
  id: string;
  name: string;
  model: string;
  baseUrl: string;
  apiBackend: string;
  apiKey?: string;
}

const PROVIDERS_KEY = "providers";
const PROVIDER_KEYS_KEY = "providerKeys";

function makeId(): string {
  return `provider-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
}

export function sanitizeProvider(raw: unknown): Provider | null {
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  const id = typeof r.id === "string" && r.id ? r.id : makeId();
  return {
    id,
    name: typeof r.name === "string" ? r.name : id,
    model: typeof r.model === "string" ? r.model : "",
    baseUrl: typeof r.baseUrl === "string" ? r.baseUrl : "",
    apiBackend: typeof r.apiBackend === "string" ? r.apiBackend : "openai",
    apiKey: typeof r.apiKey === "string" ? r.apiKey : undefined,
  };
}

export async function loadProviders(): Promise<Provider[]> {
  const list = persistGet<unknown[]>(PROVIDERS_KEY) ?? [];
  const keys = (await secureGetJson<Record<string, string>>(PROVIDER_KEYS_KEY)) ?? {};
  return list
    .map(sanitizeProvider)
    .filter((p): p is Provider => p !== null)
    .map((p) => ({ ...p, apiKey: keys[p.id] }));
}

export async function saveProviders(list: Provider[]): Promise<void> {
  const withoutKeys = list.map(({ apiKey, ...rest }) => rest);
  persistSet(PROVIDERS_KEY, withoutKeys);
  const keys: Record<string, string> = {};
  for (const p of list) {
    if (p.apiKey) keys[p.id] = p.apiKey;
  }
  await secureSetJson(PROVIDER_KEYS_KEY, keys);
}

export function providerEnvKey(backend: string): string {
  switch (backend.toLowerCase()) {
    case "xai":
    case "grok":
      return "XAI_API_KEY";
    case "openai":
      return "OPENAI_API_KEY";
    case "anthropic":
      return "ANTHROPIC_API_KEY";
    case "google":
    case "gemini":
      return "GOOGLE_API_KEY";
    case "openrouter":
      return "OPENROUTER_API_KEY";
    case "groq":
      return "GROQ_API_KEY";
    case "deepseek":
      return "DEEPSEEK_API_KEY";
    case "mistral":
      return "MISTRAL_API_KEY";
    default:
      return `${backend.toUpperCase().replace(/[^A-Z0-9]/g, "_")}_API_KEY`;
  }
}

export function findProviderApiKey(
  providers: Provider[],
  modelId: string,
  authMethodId?: string
): { key: string; value: string } | undefined {
  const backend = authMethodId?.replace(".api_key", "").toLowerCase();
  const provider = providers.find((p) => {
    if (!p.apiKey) return false;
    if (backend && (p.apiBackend === backend || p.model.toLowerCase().startsWith(backend))) return true;
    return p.model && modelId.toLowerCase().includes(p.model.toLowerCase());
  });
  if (!provider?.apiKey) return undefined;
  return { key: providerEnvKey(provider.apiBackend), value: provider.apiKey };
}
