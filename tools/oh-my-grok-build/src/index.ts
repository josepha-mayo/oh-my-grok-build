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
  scheduleStartCommand,
  scheduleStopDaemonCommand,
  scheduleStatusCommand,
  scheduleDaemonCommand,
} from "./commands/schedule.js";
import {
  toolsListCommand,
  toolsEnableCommand,
  toolsDisableCommand,
  toolsAddCommand,
  toolsRemoveCommand,
} from "./commands/tools.js";
import {
  subagentSpawnCommand,
  subagentListCommand,
  subagentKillCommand,
  subagentLogsCommand,
  subagentTraceCommand,
} from "./commands/subagent.js";
import { harnessAddCommand, harnessListCommand, harnessRemoveCommand, harnessRunCommand } from "./commands/harness.js";
import { devinAutonomousCommand } from "./commands/devin.js";
import { cronCommand } from "./commands/cron.js";
import { timelineCommand } from "./commands/timeline.js";
import { researchCommand } from "./commands/research.js";
import { browserCommand, computerCommand } from "./commands/use.js";
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
      .option("-i, --id <id>", "Provider id")
      .option("-u, --base-url <url>", "API base URL")
      .option("-m, --model <model>", "Model id")
      .option("-b, --api-backend <backend>", "API backend (chat_completions | responses | messages)")
      .option("--non-interactive", "Do not prompt; requires --id, --base-url, and --model for custom providers")
      .action(async (preset, options) => {
        await providerAddCommand({
          interactive: !options.nonInteractive,
          presetId: preset,
          id: options.id,
          baseUrl: options.baseUrl,
          model: options.model,
          apiBackend: options.apiBackend,
        });
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
  .command("research <topic>")
  .description("Deep-research an arXiv topic and synthesize a report with a proposed patch")
  .option("-n, --count <n>", "Number of papers to fetch", parseInt, 5)
  .option("-m, --model <model>", "Model to use")
  .option("--yolo", "Auto-approve tool calls")
  .action(async (topic, options) => {
    await researchCommand({ topic, ...options });
  });

program
  .command("use <prompt>")
  .description("Run a computer-use prompt (desktop + browser via MCP)")
  .option("-m, --model <model>", "Model to use")
  .option("--yolo", "Auto-approve tool calls")
  .option("--max-turns <n>", "Maximum agent turns", parseInt)
  .option("--cwd <cwd>", "Working directory")
  .action(async (prompt, options) => {
    await computerCommand({ prompt, ...options });
  });

program
  .command("browser <prompt>")
  .description("Run a browser-use prompt (browser automation via MCP)")
  .option("-m, --model <model>", "Model to use")
  .option("--yolo", "Auto-approve tool calls")
  .option("--max-turns <n>", "Maximum agent turns", parseInt)
  .option("--cwd <cwd>", "Working directory")
  .action(async (prompt, options) => {
    await browserCommand({ prompt, ...options });
  });

program
  .command("loop <prompt>")
  .description("Iteratively run a prompt until the working tree is clean")
  .option("-m, --model <model>", "Model to use")
  .option("--yolo", "Auto-approve tool calls")
  .option("--max-iterations <n>", "Maximum iterations", parseInt, 5)
  .option("--cwd <cwd>", "Working directory")
  .action(async (prompt, options) => {
    await loopCommand({ prompt, ...options });
  });

program
  .command("timeline")
  .description("Show recent timeline events")
  .option("-n, --count <n>", "Number of events to show", parseInt, 50)
  .option("-t, --type <type>", "Filter by event type")
  .action(async (options) => {
    timelineCommand(options);
  });

program
  .command("cron <expression> <prompt>")
  .description("Schedule a prompt to run on a cron expression")
  .option("-n, --name <name>", "Job name", "cron")
  .option("-m, --model <model>", "Model to use")
  .option("--yolo", "Auto-approve tool calls")
  .option("--cwd <cwd>", "Working directory for the prompt")
  .option("--foreground", "Run the scheduler in the foreground instead of the background daemon")
  .action(async (expression, prompt, options) => {
    await cronCommand({
      expression,
      prompt,
      name: options.name,
      model: options.model,
      yolo: options.yolo,
      cwd: options.cwd,
      daemon: !options.foreground,
    });
  });

program
  .command("autonomous <prompt>")
  .description("Run a prompt in fully autonomous (yolo) mode")
  .option("-m, --model <model>", "Model to use")
  .option("--sandbox-profile <profile>", "Sandbox profile to set for the grok process")
  .option("--cwd <cwd>", "Working directory")
  .action(async (prompt, options) => {
    await devinAutonomousCommand({ prompt, ...options });
  });

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

schedule.addCommand(
  new Command("start").description("Start the persistent scheduler daemon").action(async () => {
    await scheduleStartCommand();
  })
);

schedule.addCommand(
  new Command("stop-daemon")
    .alias("stopd")
    .description("Stop the persistent scheduler daemon")
    .action(async () => {
      await scheduleStopDaemonCommand();
    })
);

schedule.addCommand(
  new Command("status")
    .alias("st")
    .description("Show scheduler daemon and job status")
    .action(async () => {
      await scheduleStatusCommand();
    })
);

schedule.addCommand(
  new Command("daemon").description("Run the scheduler daemon (internal)").action(async () => {
    await scheduleDaemonCommand();
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

subagent.addCommand(
  new Command("trace")
    .description("Show subagent chat trace")
    .argument("<name>")
    .option("-n, --lines <n>", "Number of lines to show", parseInt, 50)
    .action(async (name, options) => {
      await subagentTraceCommand(name, options.lines);
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

const tools = program.command("tools").description("Manage MCP tool servers (memory, browser, computer)");

tools.addCommand(
  new Command("list")
    .alias("ls")
    .description("List configured MCP tool servers")
    .action(async () => {
      await toolsListCommand();
    })
);

tools.addCommand(
  new Command("enable")
    .description("Enable a built-in MCP tool server")
    .argument("<name>")
    .action(async (name) => {
      await toolsEnableCommand(name);
    })
);

tools.addCommand(
  new Command("disable")
    .description("Disable a built-in MCP tool server")
    .argument("<name>")
    .action(async (name) => {
      await toolsDisableCommand(name);
    })
);

tools.addCommand(
  new Command("add")
    .description("Add a custom MCP server")
    .argument("<name>")
    .argument("<command>")
    .argument("[args...]")
    .option("-e, --env <var>", "Set env var (NAME=VALUE)", collect, [])
    .action(async (name, command, args, options) => {
      const extra = Array.isArray(args) ? args : [];
      await toolsAddCommand(name, command, extra, options);
    })
);

tools.addCommand(
  new Command("remove")
    .alias("rm")
    .description("Remove a custom MCP server")
    .argument("<name>")
    .action(async (name) => {
      await toolsRemoveCommand(name);
    })
);

function collect(value: string, previous: string[]): string[] {
  return previous.concat([value]);
}

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
