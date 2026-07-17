import readline from "node:readline/promises";
import chalk from "chalk";
import { addProvider, listProviders, removeProvider, setDefaultProvider } from "../providers/manager.js";
import { listProviderTemplates, getProviderTemplate } from "../providers/registry.js";
import type { ProviderConfig } from "../types.js";

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

    const id = templateId === "custom-openai" || templateId === undefined
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
      const key = await rl.question(`${template.apiKeyLabel}${template.id === "ollama" || template.id === "lmstudio" ? " (optional)" : ""}: `);
      apiKey = key.trim() || undefined;
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
    console.log(`    envKey:  ${Array.isArray(p.envKey) ? p.envKey.join(", ") : p.envKey ?? "none"}`);
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
