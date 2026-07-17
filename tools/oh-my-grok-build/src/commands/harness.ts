import chalk from "chalk";
import {
  addConnector,
  buildConnector,
  getConnector,
  listConnectors,
  removeConnector,
} from "../integrations/manager.js";

export async function harnessAddCommand(
  name: string,
  type: "opencode" | "codex" | "claude",
  options: { url?: string; command?: string; cwd?: string; secret?: string }
): Promise<void> {
  await addConnector({ name, type, ...options });
  console.log(chalk.green(`Connector '${name}' added (${type}).`));
}

export async function harnessListCommand(): Promise<void> {
  const connectors = await listConnectors();
  if (connectors.length === 0) {
    console.log(chalk.yellow("No harness connectors configured."));
    return;
  }
  console.log(chalk.bold("\nConfigured harness connectors:\n"));
  for (const c of connectors) {
    console.log(`  ${chalk.cyan(c.name)}  ${c.type}`);
    if (c.url) console.log(`    url: ${c.url}`);
    if (c.command) console.log(`    command: ${c.command}`);
    if (c.cwd) console.log(`    cwd: ${c.cwd}`);
  }
}

export async function harnessRemoveCommand(name: string): Promise<void> {
  await removeConnector(name);
  console.log(chalk.green(`Connector '${name}' removed.`));
}

export async function harnessRunCommand(name: string, prompt: string): Promise<void> {
  const cfg = await getConnector(name);
  if (!cfg) throw new Error(`Connector '${name}' not found`);
  const connector = buildConnector(cfg);
  const result = await connector.run(prompt);
  await connector.close?.();
  console.log(result.text);
}
