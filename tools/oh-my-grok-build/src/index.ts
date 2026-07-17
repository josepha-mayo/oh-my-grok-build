#!/usr/bin/env node
import { Command } from "commander";
import chalk from "chalk";
import { serveCommand } from "./commands/serve.js";
import { connectCommand } from "./commands/connect.js";
import { providerAddCommand, providerListCommand, providerRemoveCommand, providerDefaultCommand } from "./commands/provider.js";
import { modelCommand, modelsCommand } from "./commands/model.js";
import { execCommand } from "./commands/exec.js";
import { teamCommand } from "./commands/team.js";

const program = new Command();

program
  .name("omgb")
  .description("Productivity and orchestration layer for Grok Build")
  .version("0.1.0");

program
  .command("serve")
  .description("Start the Grok Build agent server and print a mobile pairing QR code")
  .option("-b, --bind <addr>", "Bind address", "0.0.0.0")
  .option("-p, --port <port>", "Port (0 for auto)", parseInt, 0)
  .option("-s, --secret <secret>", "Server secret")
  .option("--cwd <cwd>", "Working directory")
  .option("-m, --model <model>", "Model to use")
  .option("--yolo", "Auto-approve tool calls")
  .option("--no-qr", "Do not print QR code")
  .action(async (options) => {
    await serveCommand({ ...options, qr: options.qr !== false });
  });

program
  .command("connect <url>")
  .description("Connect to a Grok agent server as a CLI client")
  .option("--cwd <cwd>", "Working directory")
  .option("--yolo", "Auto-approve tool calls")
  .action(async (url, options) => {
    await connectCommand({ url, ...options });
  });

program
  .command("provider")
  .description("Manage BYOK model providers")
  .addCommand(
    new Command("add")
      .description("Add a new provider")
      .argument("[preset]", "Provider preset id")
      .action(async (preset) => {
        await providerAddCommand(true, preset);
      })
  )
  .addCommand(
    new Command("list")
      .alias("ls")
      .description("List configured providers")
      .action(async () => {
        await providerListCommand();
      })
  )
  .addCommand(
    new Command("remove")
      .alias("rm")
      .description("Remove a provider")
      .argument("<id>")
      .action(async (id) => {
        await providerRemoveCommand(id);
      })
  )
  .addCommand(
    new Command("default")
      .description("Set the default provider")
      .argument("<id>")
      .action(async (id) => {
        await providerDefaultCommand(id);
      })
  );

program
  .command("model [model]")
  .description("Set or show the default model")
  .action(async (model) => {
    await modelCommand(model);
  });

program
  .command("models")
  .description("List configured models")
  .action(async () => {
    await modelsCommand();
  });

program
  .command("exec <prompt>")
  .description("Run a single headless Grok prompt")
  .option("-m, --model <model>", "Model to use")
  .option("--yolo", "Auto-approve tool calls")
  .option("--max-turns <n>", "Maximum agent turns", parseInt)
  .action(async (prompt, options) => {
    await execCommand({ prompt, ...options });
  });

program
  .command("team <count> <prompt>")
  .description("Spawn N Grok workers with the same prompt")
  .option("-m, --model <model>", "Model to use")
  .option("--yolo", "Auto-approve tool calls")
  .action(async (count: string, prompt: string, options) => {
    await teamCommand({ count: parseInt(count, 10), prompt, ...options });
  });

program.hook("postAction", () => {
  // Ensure async errors are not swallowed.
});

async function main() {
  try {
    await program.parseAsync(process.argv);
  } catch (err) {
    console.error(chalk.red(err instanceof Error ? err.message : String(err)));
    process.exit(1);
  }
}

main();
