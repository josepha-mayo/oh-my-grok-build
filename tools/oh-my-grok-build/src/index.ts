#!/usr/bin/env node
import { Command } from "commander";
import chalk from "chalk";
import { serveCommand } from "./commands/serve.js";
import { connectCommand } from "./commands/connect.js";
import {
  providerAddCommand,
  providerListCommand,
  providerRemoveCommand,
  providerDefaultCommand,
  providerDiscoverCommand,
  providerTestCommand,
} from "./commands/provider.js";
import { modelCommand, modelsCommand } from "./commands/model.js";
import { execCommand } from "./commands/exec.js";
import { teamCommand } from "./commands/team.js";
import { loopCommand } from "./commands/loop.js";
import {
  scheduleListCommand,
  scheduleStopCommand,
  scheduleRunCommand,
  scheduleDeleteCommand,
} from "./commands/schedule.js";
import {
  subagentSpawnCommand,
  subagentListCommand,
  subagentKillCommand,
  subagentLogsCommand,
} from "./commands/subagent.js";
import { harnessAddCommand, harnessListCommand, harnessRemoveCommand, harnessRunCommand } from "./commands/harness.js";
import { devinLoopCommand, devinAutonomousCommand } from "./commands/devin.js";
import { swarmCommand } from "./commands/swarm.js";
import { loadOmgDotEnvIntoProcess } from "./config.js";

const program = new Command();

program.name("omgb").description("Productivity and orchestration layer for Grok Build").version("0.1.0");

program
  .command("serve")
  .description("Start the Grok Build agent server and print a mobile pairing QR code")
  .option("-b, --bind <addr>", "Bind address", "127.0.0.1")
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
  .option("-m, --model <model>", "Model to use for this session")
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
  )
  .addCommand(
    new Command("discover").description("Auto-discover Ollama and LM Studio local models").action(async () => {
      await providerDiscoverCommand();
    })
  )
  .addCommand(
    new Command("test")
      .description("Test connectivity to a provider")
      .argument("<id>")
      .action(async (id) => {
        await providerTestCommand(id);
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

program
  .command("swarm <prompt>")
  .description("Decompose a task and run subagents in parallel")
  .option("-m, --model <model>", "Model to use")
  .option("--yolo", "Auto-approve tool calls")
  .option("-w, --workers <n>", "Maximum number of subagents", parseInt, 4)
  .option("-t, --timeout <ms>", "Timeout in milliseconds", parseInt, 10 * 60 * 1000)
  .option("--max-turns <n>", "Maximum agent turns", parseInt)
  .action(async (prompt, options) => {
    await swarmCommand({ prompt, ...options });
  });

program
  .command("loop <expression> <prompt>")
  .description("Run a prompt on a cron schedule")
  .option("-m, --model <model>", "Model to use")
  .option("--yolo", "Auto-approve tool calls")
  .action(async (expression, prompt, options) => {
    await loopCommand({ expression, prompt, ...options });
  });

const devin = program.command("devin").description("Devin-style loop and autonomous modes");

devin.addCommand(
  new Command("loop")
    .description("Iteratively run a prompt until the working tree is clean")
    .argument("<prompt>")
    .option("-m, --model <model>", "Model to use")
    .option("--yolo", "Auto-approve tool calls")
    .option("--max-iterations <n>", "Maximum iterations", parseInt, 5)
    .option("--cwd <cwd>", "Working directory")
    .action(async (prompt, options) => {
      await devinLoopCommand({ prompt, ...options });
    })
);

devin.addCommand(
  new Command("autonomous")
    .description("Run a prompt in fully autonomous (yolo) mode")
    .argument("<prompt>")
    .option("-m, --model <model>", "Model to use")
    .option("--sandbox-profile <profile>", "Sandbox profile to set for the grok process")
    .option("--cwd <cwd>", "Working directory")
    .action(async (prompt, options) => {
      await devinAutonomousCommand({ prompt, ...options });
    })
);

const schedule = program.command("schedule").description("Manage scheduled background jobs");

schedule.addCommand(
  new Command("list")
    .alias("ls")
    .description("List scheduled jobs")
    .action(async () => {
      await scheduleListCommand();
    })
);

schedule.addCommand(
  new Command("stop")
    .description("Stop a scheduled job")
    .argument("<name>")
    .action(async (name) => {
      await scheduleStopCommand(name);
    })
);

schedule.addCommand(
  new Command("run")
    .description("Run a scheduled job now")
    .argument("<name>")
    .action(async (name) => {
      await scheduleRunCommand(name);
    })
);

schedule.addCommand(
  new Command("delete")
    .alias("rm")
    .description("Delete a scheduled job")
    .argument("<name>")
    .action(async (name) => {
      await scheduleDeleteCommand(name);
    })
);

const subagent = program.command("subagent").description("Spawn and manage Grok subagents");

subagent.addCommand(
  new Command("spawn")
    .description("Spawn a detached Grok subagent in a worktree")
    .argument("<name>")
    .argument("<prompt>")
    .option("-m, --model <model>", "Model to use")
    .option("--yolo", "Auto-approve tool calls")
    .option("--max-turns <n>", "Maximum agent turns", parseInt)
    .action(async (name, prompt, options) => {
      await subagentSpawnCommand({ name, prompt, ...options });
    })
);

subagent.addCommand(
  new Command("list")
    .alias("ls")
    .description("List subagents")
    .action(async () => {
      await subagentListCommand();
    })
);

subagent.addCommand(
  new Command("kill")
    .description("Kill a subagent")
    .argument("<name>")
    .action(async (name) => {
      await subagentKillCommand(name);
    })
);

subagent.addCommand(
  new Command("logs")
    .description("Show subagent logs")
    .argument("<name>")
    .option("-n, --lines <n>", "Number of lines to show", parseInt, 50)
    .action(async (name, options) => {
      await subagentLogsCommand(name, options.lines);
    })
);

const harness = program
  .command("harness")
  .description("Connect and run other agent harnesses (OpenCode, Codex, Claude, Hermes, Pi, OMP)");

harness.addCommand(
  new Command("add")
    .description("Add a connector to another harness")
    .argument("<name>")
    .argument("<type>")
    .option("--url <url>", "Server URL (for opencode)")
    .option("--command <command>", "Binary or command override")
    .option("--cwd <cwd>", "Working directory")
    .option("--secret <secret>", "Auth secret")
    .action(async (name, type, options) => {
      await harnessAddCommand(name, type, options);
    })
);

harness.addCommand(
  new Command("list")
    .alias("ls")
    .description("List harness connectors")
    .action(async () => {
      await harnessListCommand();
    })
);

harness.addCommand(
  new Command("remove")
    .alias("rm")
    .description("Remove a harness connector")
    .argument("<name>")
    .action(async (name) => {
      await harnessRemoveCommand(name);
    })
);

harness.addCommand(
  new Command("run")
    .description("Run a prompt through a harness connector")
    .argument("<name>")
    .argument("<prompt>")
    .action(async (name, prompt) => {
      await harnessRunCommand(name, prompt);
    })
);

async function main() {
  try {
    await loadOmgDotEnvIntoProcess();
    await program.parseAsync(process.argv);
  } catch (err) {
    console.error(chalk.red(err instanceof Error ? err.message : String(err)));
    process.exit(1);
  }
}

main();
