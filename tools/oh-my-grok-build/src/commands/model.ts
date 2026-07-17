import chalk from "chalk";
import { loadGrokConfig, saveGrokConfig, loadOmgConfig, saveOmgConfig } from "../config.js";

export async function modelCommand(modelId?: string): Promise<void> {
  if (!modelId) {
    const cfg = await loadGrokConfig();
    const models = cfg.models as Record<string, unknown> | undefined;
    console.log(chalk.bold(`Current default model: ${chalk.cyan(String(models?.default ?? "grok-build"))}`));
    return;
  }

  // Update Grok config default.
  const gcfg = await loadGrokConfig();
  if (!gcfg.models || typeof gcfg.models !== "object" || Array.isArray(gcfg.models)) {
    gcfg.models = {};
  }
  (gcfg.models as Record<string, unknown>).default = modelId;
  await saveGrokConfig(gcfg);

  // Update OMGB config default.
  const ocfg = await loadOmgConfig();
  ocfg.defaultModel = modelId;
  await saveOmgConfig(ocfg);

  console.log(chalk.green(`Default model set to ${chalk.cyan(modelId)}`));
}

export async function modelsCommand(): Promise<void> {
  const cfg = await loadGrokConfig();
  const models = cfg.models as Record<string, unknown> | undefined;
  const modelTable = (cfg.model ?? {}) as Record<string, Record<string, unknown>>;

  console.log(chalk.bold(`Default model: ${chalk.cyan(String(models?.default ?? "grok-build"))}\n`));
  const entries = Object.entries(modelTable);
  if (entries.length === 0) {
    console.log(chalk.yellow("No custom models in ~/.grok/config.toml."));
    return;
  }
  console.log(chalk.bold("Custom models:"));
  for (const [id, section] of entries) {
    console.log(`  ${chalk.cyan(id)}  ${section.name ?? ""}  (${section.model ?? ""})`);
  }
}
