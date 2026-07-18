import readline from "node:readline/promises";
import chalk from "chalk";
import { DEFAULT_MODEL, loadGrokConfig, saveGrokConfig, loadOmgConfig, saveOmgConfig } from "../config.js";

interface SelectableModel {
  id: string;
  label: string;
}

async function listSelectableModels(): Promise<SelectableModel[]> {
  const models: SelectableModel[] = [{ id: DEFAULT_MODEL, label: "Grok 4.5 (built-in)" }];

  const ocfg = await loadOmgConfig();
  for (const p of Object.values(ocfg.providers ?? {})) {
    models.push({ id: `omgb-${p.id}`, label: `${p.name} (${p.model})` });
  }

  const gcfg = await loadGrokConfig();
  const modelTable = (gcfg.model ?? {}) as Record<string, Record<string, unknown>>;
  for (const [id, section] of Object.entries(modelTable)) {
    if (models.some((m) => m.id === id)) continue;
    models.push({ id, label: `${section.name ?? id} (${section.model ?? ""})` });
  }

  return models;
}

async function pickModel(rl: readline.Interface, models: SelectableModel[], current?: string): Promise<string> {
  console.log(chalk.bold("\nAvailable models:"));
  models.forEach((m, i) => {
    const marker = current && m.id === current ? chalk.yellow(" *") : "";
    console.log(`  ${i + 1}. ${m.label}${marker}`);
  });

  const answer = (await rl.question("\nPick a model (number or id): ")).trim();
  if (/^\d+$/.test(answer)) {
    const idx = parseInt(answer, 10) - 1;
    if (idx >= 0 && idx < models.length) return models[idx].id;
  }
  if (answer) return answer;
  return current ?? DEFAULT_MODEL;
}

async function interactiveSetModel(): Promise<void> {
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  try {
    const gcfg = await loadGrokConfig();
    const current = (gcfg.models as Record<string, unknown> | undefined)?.default as string | undefined;
    const models = await listSelectableModels();
    const selected = await pickModel(rl, models, current);
    await setDefaultModel(selected);
  } finally {
    rl.close();
  }
}

async function setDefaultModel(modelId: string): Promise<void> {
  const gcfg = await loadGrokConfig();
  if (!gcfg.models || typeof gcfg.models !== "object" || Array.isArray(gcfg.models)) {
    gcfg.models = {};
  }
  (gcfg.models as Record<string, unknown>).default = modelId;
  await saveGrokConfig(gcfg);

  const ocfg = await loadOmgConfig();
  ocfg.defaultModel = modelId;
  await saveOmgConfig(ocfg);

  console.log(chalk.green(`Default model set to ${chalk.cyan(modelId)}`));
}

export async function modelCommand(modelId?: string): Promise<void> {
  if (!modelId) {
    const gcfg = await loadGrokConfig();
    const current = (gcfg.models as Record<string, unknown> | undefined)?.default;
    console.log(chalk.bold(`Current default model: ${chalk.cyan(String(current ?? DEFAULT_MODEL))}`));
    if (process.stdin.isTTY && process.stdout.isTTY) {
      await interactiveSetModel();
    }
    return;
  }

  await setDefaultModel(modelId);
}

export async function modelsCommand(): Promise<void> {
  const gcfg = await loadGrokConfig();
  const current = (gcfg.models as Record<string, unknown> | undefined)?.default;
  const models = await listSelectableModels();

  console.log(chalk.bold(`Default model: ${chalk.cyan(String(current ?? DEFAULT_MODEL))}\n`));
  console.log(chalk.bold("Available models:"));
  for (const m of models) {
    const marker = current && m.id === current ? chalk.yellow(" *") : "";
    console.log(`  ${chalk.cyan(m.id)}  ${m.label}${marker}`);
  }
}
