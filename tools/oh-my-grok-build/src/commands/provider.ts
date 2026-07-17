import readline from "node:readline/promises";
import { Writable } from "node:stream";
import chalk from "chalk";
import { addProvider, getProvider, listProviders, removeProvider, setDefaultProvider } from "../providers/manager.js";
import { listProviderTemplates, getProviderTemplate } from "../providers/registry.js";
import { discoverLocalModels, testProvider } from "../providers/local.js";
import type { ProviderConfig } from "../types.js";

async function questionHidden(query: string): Promise<string> {
  process.stdout.write(query);
  const rl = readline.createInterface({
    input: process.stdin,
    output: new Writable({
      write(_chunk, _encoding, callback) {
        callback();
      },
    }),
    terminal: true,
  });
  try {
    const answer = await rl.question("");
    return answer.trim();
  } finally {
    rl.close();
    process.stdout.write("\n");
  }
}

export async function providerAddCommand(interactive = true, presetId?: string): Promise<void> {
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });

  try {
    let templateId = presetId;
    if (interactive && !templateId) {
      const templates = listProviderTemplates();
      console.log(chalk.bold("\nAvailable providers:"));
      templates.forEach((t, i) => console.log(`  ${i + 1}. ${t.id} - ${t.name}`));
      const choice = await rl.question("Pick a provider (number or id): ");
      const byIndex = templates[parseInt(choice, 10) - 1];
      templateId = byIndex?.id ?? choice.trim().toLowerCase();
    }

    const template = getProviderTemplate(templateId ?? "custom-openai");
    if (!template) throw new Error(`Unknown provider template: ${templateId}`);

    const id =
      templateId === "custom-openai" || templateId === undefined
        ? (await rl.question("Provider id (e.g. my-corp): ")).trim()
        : templateId;

    const baseUrl = template.baseUrl
      ? template.baseUrl
      : (await rl.question("API base URL (OpenAI-compatible): ")).trim();

    const defaultModel = template.defaultModel
      ? template.defaultModel
      : (await rl.question("Default model id: ")).trim();

    let apiKey: string | undefined;
    if (template.apiKeyLabel) {
      const suffix = template.id === "ollama" || template.id === "lmstudio" ? " (optional)" : "";
      const key = await questionHidden(`${template.apiKeyLabel}${suffix}: `);
      apiKey = key || undefined;
    }

    const model = defaultModel || (await rl.question("Model id: ")).trim();

    const provider = await addProvider({
      id,
      name: template.name,
      model,
      baseUrl,
      apiBackend: template.apiBackend,
      apiKey,
      envKey: template.envKey,
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

    const envKey = Array.isArray(template.envKey)
      ? template.envKey.filter((k) => k.endsWith("_API_KEY"))
      : template.envKey?.endsWith("_API_KEY")
        ? [template.envKey]
        : undefined;

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
