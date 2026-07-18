import readline from "node:readline/promises";
import { Writable } from "node:stream";
import chalk from "chalk";
import { addProvider, getProvider, listProviders, removeProvider, setDefaultProvider } from "../providers/manager.js";
import { listProviderTemplates, getProviderTemplate } from "../providers/registry.js";
import { discoverLocalModels, testProvider, fetchModelList } from "../providers/local.js";
import type { ProviderConfig } from "../types.js";

export interface ProviderAddOptions {
  interactive?: boolean;
  presetId?: string;
  id?: string;
  baseUrl?: string;
  model?: string;
  apiKey?: string;
  apiBackend?: ProviderConfig["apiBackend"];
}

function pickProviderEnvKey(
  template: { id: string; baseUrl: string; envKey?: string | string[] },
  baseUrl: string,
  apiKey?: string
): string | string[] | undefined {
  if (apiKey) return undefined;
  if (baseUrl.startsWith("http://localhost") || baseUrl.startsWith("https://localhost")) return undefined;
  if (!template.envKey) return undefined;
  if (typeof template.envKey === "string") return template.envKey;
  const filtered = template.envKey.filter((k) => k.endsWith("_API_KEY"));
  return filtered.length ? filtered : undefined;
}

async function questionHidden(rl: readline.Interface, query: string): Promise<string> {
  process.stdout.write(query);
  rl.pause();
  const hidden = readline.createInterface({
    input: process.stdin,
    output: new Writable({
      write(_chunk, _encoding, callback) {
        callback();
      },
    }),
    terminal: true,
  });
  try {
    const answer = await hidden.question("");
    return answer.trim();
  } finally {
    hidden.close();
    rl.resume();
    process.stdout.write("\n");
  }
}

async function chooseModel(
  rl: readline.Interface,
  baseUrl: string,
  apiKey: string | undefined,
  apiBackend: string,
  extraHeaders: Record<string, string> = {},
  defaultValue = ""
): Promise<string> {
  const models = await fetchModelList(baseUrl, apiKey, apiBackend, extraHeaders);

  if (models && models.length > 0) {
    console.log(chalk.bold("\nAvailable models:"));
    models.forEach((m, i) => console.log(`  ${i + 1}. ${m}`));
    const choice = (await rl.question("\nPick a model (number or id): ")).trim();
    if (/^\d+$/.test(choice)) {
      const idx = parseInt(choice, 10) - 1;
      if (idx >= 0 && idx < models.length) return models[idx];
    }
    if (choice) return choice;
  }

  const prompt = defaultValue ? `Model id [${defaultValue}]: ` : "Model id: ";
  const answer = (await rl.question(prompt)).trim();
  return answer || defaultValue;
}

export async function providerAddCommand(options: ProviderAddOptions = {}): Promise<void> {
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });

  try {
    let templateId = options.presetId;
    if (options.interactive !== false && !templateId) {
      const templates = listProviderTemplates();
      console.log(chalk.bold("\nAvailable providers:"));
      templates.forEach((t, i) => console.log(`  ${i + 1}. ${t.id} - ${t.name}`));
      const choice = await rl.question("Pick a provider (number or id): ");
      const byIndex = templates[parseInt(choice, 10) - 1];
      templateId = byIndex?.id ?? choice.trim().toLowerCase();
    }

    const template = getProviderTemplate(templateId ?? "custom-openai");
    if (!template) throw new Error(`Unknown provider template: ${templateId}`);

    const isCustom = template.id === "custom-openai";

    let id: string;
    if (options.id) {
      id = options.id;
    } else if (isCustom) {
      if (options.interactive === false) throw new Error("--id is required for custom providers");
      id = (await rl.question("Provider id (e.g. my-corp): ")).trim();
    } else {
      id = template.id;
    }
    if (!id) throw new Error("Provider id is required");

    let baseUrl: string;
    if (options.baseUrl) {
      baseUrl = options.baseUrl;
    } else if (template.baseUrl) {
      baseUrl = template.baseUrl;
    } else if (options.interactive !== false) {
      baseUrl = (await rl.question("API base URL (OpenAI-compatible): ")).trim();
    } else {
      throw new Error("--base-url is required");
    }
    if (!baseUrl) throw new Error("API base URL is required");

    const apiBackend = options.apiBackend ?? template.apiBackend ?? "chat_completions";

    let apiKey: string | undefined;
    if (options.apiKey !== undefined) {
      apiKey = options.apiKey || undefined;
    } else if (options.interactive !== false) {
      const suffix = " (leave blank to use env var or for no auth)";
      const key = template.apiKeyLabel
        ? await questionHidden(rl, `${template.apiKeyLabel}${suffix}: `)
        : await questionHidden(rl, `API key${suffix}: `);
      apiKey = key || undefined;
    }

    let model: string;
    if (options.model) {
      model = options.model;
    } else if (options.interactive !== false) {
      model = await chooseModel(rl, baseUrl, apiKey, apiBackend, template.extraHeaders, template.defaultModel ?? "");
    } else if (template.defaultModel) {
      model = template.defaultModel;
    } else {
      throw new Error("--model is required");
    }
    if (!model) throw new Error("Model id is required");

    const provider = await addProvider({
      id,
      name: template.name,
      model,
      baseUrl,
      apiBackend,
      apiKey,
      envKey: pickProviderEnvKey(template, baseUrl, apiKey),
      extraHeaders: template.extraHeaders,
      contextWindow: template.contextWindow,
    });

    console.log(chalk.green(`\nProvider '${provider.id}' added and synced to Grok config.`));
    console.log(chalk.dim(`  Model: omgb-${provider.id}`));
  } finally {
    rl.close();
  }
}

export async function providerListCommand(): Promise<void> {
  const providers = await listProviders();
  if (providers.length === 0) {
    console.log(chalk.yellow("No providers configured. Run `omgb provider add`."));
    return;
  }
  console.log(chalk.bold("\nConfigured providers:\n"));
  for (const p of providers) {
    console.log(`  ${chalk.cyan(`omgb-${p.id}`)}  ${p.name}`);
    console.log(`    model:   ${p.model}`);
    console.log(`    base:    ${p.baseUrl}`);
    console.log(`    backend: ${p.apiBackend ?? "chat_completions"}`);
    console.log(`    envKey:  ${Array.isArray(p.envKey) ? p.envKey.join(", ") : (p.envKey ?? "none")}`);
    console.log("");
  }
}

export async function providerRemoveCommand(id: string): Promise<void> {
  await removeProvider(id);
  console.log(chalk.green(`Provider '${id}' removed.`));
}

export async function providerDefaultCommand(id: string): Promise<void> {
  await setDefaultProvider(id);
  console.log(chalk.green(`Default model set to omgb-${id}.`));
}

function sanitizeModelId(id: string): string {
  return id
    .toLowerCase()
    .replace(/[^a-z0-9_-]/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
}

export async function providerDiscoverCommand(): Promise<void> {
  const found = await discoverLocalModels();
  if (found.length === 0) {
    console.log(chalk.yellow("No local models discovered. Make sure Ollama or LM Studio is running."));
    return;
  }

  for (const group of found) {
    const template = getProviderTemplate(group.provider);
    if (!template) continue;

    const envKey = pickProviderEnvKey(template, template.baseUrl);

    for (const model of group.models) {
      const safeModel = sanitizeModelId(model);
      const id = `${group.provider}-${safeModel}`;
      if (await getProvider(id)) continue;

      await addProvider({
        id,
        name: `${template.name} ${model}`,
        model,
        baseUrl: template.baseUrl,
        apiBackend: "chat_completions",
        envKey,
      });

      console.log(chalk.green(`Added omgb-${id}`));
    }
  }
}

export async function providerTestCommand(id: string): Promise<void> {
  const provider = await getProvider(id);
  if (!provider) throw new Error(`Provider '${id}' not found`);
  const result = await testProvider(provider);
  if (result.ok) {
    console.log(chalk.green(`Provider '${id}' is reachable.`));
  } else {
    console.log(chalk.red(`Provider '${id}' test failed: ${result.error}`));
    process.exitCode = 1;
  }
}
