import type { ProviderConfig } from "../types.js";

export interface ProviderTemplate {
  name: string;
  baseUrl: string;
  apiBackend?: ProviderConfig["apiBackend"];
  envKey?: string | string[];
  apiKeyLabel?: string;
  defaultModel?: string;
  extraHeaders?: Record<string, string>;
  contextWindow?: number;
}

export const BUILT_IN_PROVIDERS: Record<string, ProviderTemplate> = {
  openai: {
    name: "OpenAI",
    baseUrl: "https://api.openai.com/v1",
    apiBackend: "responses",
    envKey: "OPENAI_API_KEY",
    apiKeyLabel: "OpenAI API key",
    defaultModel: "gpt-4o",
    contextWindow: 200_000,
  },
  anthropic: {
    name: "Anthropic",
    baseUrl: "https://api.anthropic.com/v1",
    apiBackend: "messages",
    envKey: "ANTHROPIC_API_KEY",
    apiKeyLabel: "Anthropic API key",
    defaultModel: "claude-3-5-sonnet-20241022",
    extraHeaders: { "anthropic-version": "2023-06-01" },
    contextWindow: 200_000,
  },
  xai: {
    name: "xAI",
    baseUrl: "https://api.x.ai/v1",
    apiBackend: "chat_completions",
    envKey: "XAI_API_KEY",
    apiKeyLabel: "xAI API key",
    defaultModel: "grok-4.5",
    contextWindow: 500_000,
  },
  openrouter: {
    name: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
    apiBackend: "chat_completions",
    envKey: "OPENROUTER_API_KEY",
    apiKeyLabel: "OpenRouter API key",
    defaultModel: "anthropic/claude-3.5-sonnet",
    extraHeaders: { "HTTP-Referer": "https://oh-my-grok.build", "X-Title": "oh-my-grok-build" },
    contextWindow: 200_000,
  },
  ollama: {
    name: "Ollama (local)",
    baseUrl: "http://localhost:11434/v1",
    apiBackend: "chat_completions",
    envKey: ["OLLAMA_HOST", "OPENAI_API_KEY"],
    apiKeyLabel: "API key (leave blank for local Ollama)",
    defaultModel: "codellama",
    contextWindow: 128_000,
  },
  lmstudio: {
    name: "LM Studio (local)",
    baseUrl: "http://localhost:1234/v1",
    apiBackend: "chat_completions",
    envKey: ["LMSTUDIO_API_KEY", "OPENAI_API_KEY"],
    apiKeyLabel: "API key (leave blank for local LM Studio)",
    defaultModel: "local-model",
    contextWindow: 128_000,
  },
  together: {
    name: "Together AI",
    baseUrl: "https://api.together.xyz/v1",
    apiBackend: "chat_completions",
    envKey: "TOGETHER_API_KEY",
    apiKeyLabel: "Together API key",
    defaultModel: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
    contextWindow: 128_000,
  },
  fireworks: {
    name: "Fireworks AI",
    baseUrl: "https://api.fireworks.ai/inference/v1",
    apiBackend: "chat_completions",
    envKey: "FIREWORKS_API_KEY",
    apiKeyLabel: "Fireworks API key",
    defaultModel: "accounts/fireworks/models/deepseek-coder-v2",
    contextWindow: 128_000,
  },
  groq: {
    name: "Groq",
    baseUrl: "https://api.groq.com/openai/v1",
    apiBackend: "chat_completions",
    envKey: "GROQ_API_KEY",
    apiKeyLabel: "Groq API key",
    defaultModel: "llama-3.1-70b-versatile",
    contextWindow: 128_000,
  },
  "local-openai": {
    name: "Local OpenAI-compatible server",
    baseUrl: "http://localhost:8000/v1",
    apiBackend: "chat_completions",
    envKey: ["OPENAI_API_KEY"],
    apiKeyLabel: "API key (leave blank for local)",
    defaultModel: "",
    contextWindow: 128_000,
  },
  vllm: {
    name: "vLLM",
    baseUrl: "http://localhost:8000/v1",
    apiBackend: "chat_completions",
    envKey: ["OPENAI_API_KEY"],
    apiKeyLabel: "API key (leave blank for local vLLM)",
    defaultModel: "",
    contextWindow: 128_000,
  },
  "llama-cpp": {
    name: "llama.cpp server",
    baseUrl: "http://localhost:8080/v1",
    apiBackend: "chat_completions",
    envKey: ["OPENAI_API_KEY"],
    apiKeyLabel: "API key (leave blank for local llama.cpp)",
    defaultModel: "",
    contextWindow: 128_000,
  },
  tabby: {
    name: "TabbyAPI",
    baseUrl: "http://localhost:5000/v1",
    apiBackend: "chat_completions",
    envKey: ["OPENAI_API_KEY"],
    apiKeyLabel: "API key (leave blank for local TabbyAPI)",
    defaultModel: "",
    contextWindow: 128_000,
  },
  "custom-openai": {
    name: "Custom OpenAI-compatible",
    baseUrl: "",
    apiBackend: "chat_completions",
    envKey: ["OPENAI_API_KEY", "XAI_API_KEY"],
    apiKeyLabel: "API key",
    defaultModel: "",
    contextWindow: 128_000,
  },
};

export type ProviderTemplateWithId = ProviderTemplate & { id: string };

export function listProviderTemplates(): ProviderTemplateWithId[] {
  return Object.entries(BUILT_IN_PROVIDERS).map(([id, t]) => ({ ...t, id }));
}

export function getProviderTemplate(id: string): ProviderTemplateWithId | undefined {
  const t = BUILT_IN_PROVIDERS[id];
  if (!t) return undefined;
  return { ...t, id };
}
