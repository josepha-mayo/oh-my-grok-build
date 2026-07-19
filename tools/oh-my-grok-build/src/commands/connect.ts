import path from "node:path";
import readline from "node:readline/promises";
import type { Readable as ReadableStream, Writable as WritableStream } from "node:stream";
import chalk from "chalk";
import { AcpClient } from "../acp/client.js";
import { createNodeWebSocketTransport } from "../acp/transport.js";
import { parseServerUrl } from "../acp/server.js";
import { isAllowedWsUrl, createWsLookup } from "../net.js";
import { isRateLimited, formatRateLimitMessage } from "../rate-limit.js";
import { DEFAULT_MODEL, loadOmgConfig } from "../config.js";
import spawner from "../spawner.js";
import { swarmCommand } from "./swarm.js";
import {
  scheduleListCommand,
  scheduleStopCommand,
  scheduleRunCommand,
  scheduleDeleteCommand,
  scheduleStartCommand,
  scheduleStopDaemonCommand,
} from "./schedule.js";
import { loadMcpConfig, toAcpMcpServers } from "../mcp/mcp-config.js";
import type { AcpNewSessionResponse, AcpPermissionRequest, AcpPermissionResponse, AcpUpdate } from "../types.js";

export interface ConnectOptions {
  url: string;
  cwd?: string;
  yolo?: boolean;
  model?: string;
  input?: ReadableStream;
  output?: WritableStream;
  exit?: (code: number) => void;
}

type ReasoningEffort = "low" | "medium" | "high" | "max";

const SLASH_COMMANDS = [
  "/help",
  "/model <model-id>",
  "/effort low|medium|high|max",
  "/yolo",
  "/autonomous",
  "/plan",
  "/loop [count] <prompt>",
  "/swarm <prompt>",
  "/schedule [list|start|stop-daemon|stop <name>|run <name>|delete <name>]",
  "/btw <note>",
  "/new",
  "/clear",
  "/quit",
  "/exit",
];

const EFFORTS: ReasoningEffort[] = ["low", "medium", "high", "max"];
const GIT_DIFF_MAX_BYTES = 100_000;
const TURN_TIMEOUT_MS = 120_000;

function formatUserError(err: unknown, label: string): string {
  const message = err instanceof Error ? err.message : String(err);
  if (isRateLimited(message)) {
    return chalk.yellow(formatRateLimitMessage());
  }
  return chalk.red(`${label}: ${message}`);
}

function gitOutput(cwd: string, args: string[], maxBytes = GIT_DIFF_MAX_BYTES): Promise<string> {
  return new Promise((resolve, reject) => {
    let output = "";
    let stderr = "";
    let killed = false;
    const proc = spawner.spawn("git", args, { cwd });
    proc.stdout?.setEncoding("utf8");
    proc.stdout?.on("data", (chunk: string) => {
      if (killed) return;
      output += chunk;
      if (Buffer.byteLength(output, "utf8") > maxBytes) {
        killed = true;
        proc.kill("SIGTERM");
        output += "\n[truncated: output exceeded size limit]";
      }
    });
    proc.stderr?.setEncoding("utf8");
    proc.stderr?.on("data", (chunk: string) => {
      stderr += chunk;
    });
    proc.on("error", (err) => reject(new Error(`git ${args.join(" ")} failed to start: ${err.message}`)));
    proc.on("exit", (code) => {
      if (code !== 0) {
        const err = new Error(
          `git ${args.join(" ")} exited with code ${code}: ${stderr.trim() || "(no stderr)"}`
        ) as Error & { code?: number | null };
        err.code = code;
        reject(err);
        return;
      }
      resolve(output);
    });
  });
}

function gitStatusShort(cwd: string): Promise<string> {
  return gitOutput(cwd, ["status", "--short"]);
}

function gitDiff(cwd: string): Promise<string> {
  return gitOutput(cwd, ["diff"]);
}

function isNotGitRepo(err: unknown): boolean {
  if (err instanceof Error) {
    if ((err as Error & { code?: number | null }).code === 128) return true;
    if (err.message.includes("not a git repository")) return true;
  }
  return false;
}

async function buildLoopPrompt(cwd: string, original: string, iteration: number, total: number): Promise<string> {
  if (iteration === 0) return original;
  if (iteration === total - 1) return `Wrap up and finalize. Original task: ${original}`;
  let status: string;
  let diff: string;
  try {
    status = await gitStatusShort(cwd);
    diff = await gitDiff(cwd).catch(() => "(could not read git diff)");
  } catch (err) {
    if (isNotGitRepo(err)) {
      status = "(not in a git repository)";
      diff = "(not in a git repository)";
    } else {
      throw err;
    }
  }
  return [
    `Original task: ${original}`,
    "",
    "Working tree status:",
    status.trim() || "(no changes)",
    "",
    "Diff:",
    diff.trim() || "(no diff)",
    "",
    "Review the result above, fix any issues, and continue.",
  ].join("\n");
}

export async function connectCommand(options: ConnectOptions): Promise<void> {
  const allowed = await isAllowedWsUrl(options.url, true);
  if (!allowed.ok) {
    throw new Error(`Cannot connect: ${allowed.reason}`);
  }

  const parsed = parseServerUrl(options.url);
  if (!parsed.secret) {
    throw new Error("URL must include a server-key query parameter, e.g. ws://host:port/ws?server-key=XYZ");
  }

  const lookup = await createWsLookup(options.url, true);
  const transport = await createNodeWebSocketTransport(
    parsed.baseUrl,
    { Authorization: `Bearer ${parsed.secret}` },
    lookup
  );

  const input = options.input ?? process.stdin;
  const output = options.output ?? process.stdout;
  const exit = options.exit ?? ((code: number) => process.exit(code));

  const rl = readline.createInterface({ input, output });

  let closing = false;
  let turnResolver: (() => void) | undefined;
  let turnRejecter: ((err: Error) => void) | undefined;

  rl.on("close", () => {
    if (closing) return;
    closing = true;
    client.close();
    exit(0);
  });

  const client = new AcpClient(transport, {
    onUpdate: (_sessionId, update) => {
      renderUpdate(update);
      if (update.sessionUpdate === "turn_completed" || update.sessionUpdate === "stop") {
        turnResolver?.();
        turnResolver = undefined;
        turnRejecter = undefined;
      }
    },
    onPermission: async (req) => handlePermission(req, rl),
    onAskUser: async ({ question }) => {
      console.log(chalk.yellow(`\n${question}`));
      return rl.question("Your answer: ");
    },
    onError: (err) => {
      console.error(chalk.red(`ACP error: ${err.message}`));
      turnRejecter?.(err);
    },
    onClose: () => {
      if (closing) return;
      console.log(chalk.dim("\nConnection closed."));
      rl.close();
      exit(0);
    },
  });

  const init = await client.initialize(1, { terminal: true, fs: { readTextFile: true, writeTextFile: true } }, 30_000);

  const authMethod = init.authMethods?.find((m) => m.id === "xai.api_key") ?? init.authMethods?.[0];
  if (authMethod) {
    await client.authenticate(authMethod, 60_000);
  }

  const cwd = path.resolve(options.cwd ?? process.cwd());
  const cfg = await loadOmgConfig();
  const explicitModel = options.model ?? cfg.defaultModel;
  let currentModel = explicitModel ?? DEFAULT_MODEL;
  let hasExplicitModel = Boolean(explicitModel);
  let currentEffort: ReasoningEffort = "medium";
  let currentYolo = options.yolo ?? false;
  let currentAuto = false;

  function buildSessionMeta(): Record<string, unknown> {
    const meta: Record<string, unknown> = {
      yoloMode: currentYolo,
      autoMode: currentAuto,
      reasoningEffort: currentEffort,
    };
    if (hasExplicitModel) meta.modelId = currentModel;
    return meta;
  }

  async function startSession(): Promise<AcpNewSessionResponse> {
    const mcpServers = toAcpMcpServers(await loadMcpConfig());
    const session = await client.newSession(cwd, mcpServers, buildSessionMeta(), 60_000);
    console.log(chalk.dim(`Session: ${session.sessionId}\n`));
    return session;
  }

  let session = await startSession();

  try {
    await applyMode(currentAuto ? "autonomous" : currentYolo ? "code" : "ask");
  } catch (err) {
    console.warn(formatUserError(err, "Could not set mode"));
  }

  if (hasExplicitModel) {
    try {
      await applyModel(currentModel);
    } catch (err) {
      console.warn(formatUserError(err, "Could not apply model/effort"));
    }
  }

  async function applyMode(desired: string): Promise<void> {
    let applied = await client.setMode(session.sessionId, desired, 10_000).catch(() => false);
    if (!applied && desired === "autonomous") {
      applied = await client.setMode(session.sessionId, "code", 10_000).catch(() => false);
      if (applied) {
        console.log(chalk.dim("Autonomous mode not available; using code mode with auto-approval classifier."));
        return;
      }
    }
    if (applied) {
      console.log(chalk.dim(`Mode set to ${desired}.`));
    } else if (desired !== "ask") {
      console.log(chalk.dim(`${desired} mode is not available on this agent.`));
    }
  }

  async function applyEffort(): Promise<void> {
    const ok = await client.setEffort(session.sessionId, currentEffort, 10_000).catch(() => false);
    if (!ok) {
      throw new Error("Reasoning effort is not configurable on this agent.");
    }
    console.log(chalk.dim(`Reasoning effort set to ${currentEffort}.`));
  }

  async function applyModel(modelId: string): Promise<void> {
    await client.setModelWithEffort(session.sessionId, modelId, currentEffort, 10_000);
    currentModel = modelId;
    hasExplicitModel = true;
    console.log(chalk.dim(`Model set to ${modelId}.`));
  }

  while (true) {
    let line: string;
    try {
      line = await rl.question(chalk.bold("you> "));
    } catch {
      if (closing) return;
      throw new Error("Readline closed unexpectedly");
    }
    if (!line.trim()) continue;
    const trimmed = line.trim();

    if (trimmed === "/help" || trimmed === "/?") {
      console.log(chalk.dim("Available commands:"));
      SLASH_COMMANDS.forEach((c) => console.log(chalk.dim(`  ${c}`)));
      continue;
    }

    if (trimmed === "/quit" || trimmed === "/exit") {
      closing = true;
      client.close();
      rl.close();
      return;
    }

    if (trimmed === "/clear") {
      console.clear();
      continue;
    }

    if (trimmed === "/new") {
      session = await startSession();
      continue;
    }

    if (trimmed.startsWith("/model")) {
      const modelId = trimmed.slice("/model".length).trim();
      if (!modelId) {
        console.log(chalk.yellow("Usage: /model <model-id>"));
      } else {
        try {
          await applyModel(modelId);
        } catch (err) {
          console.error(formatUserError(err, "Failed to set model"));
        }
      }
      continue;
    }

    if (trimmed.startsWith("/effort")) {
      const effort = trimmed.slice("/effort".length).trim() as ReasoningEffort;
      if (!EFFORTS.includes(effort)) {
        console.log(chalk.yellow("Usage: /effort low|medium|high|max"));
      } else {
        currentEffort = effort;
        try {
          await applyEffort();
        } catch (err) {
          console.error(formatUserError(err, "Failed to set effort"));
        }
      }
      continue;
    }

    if (trimmed === "/yolo") {
      currentYolo = !currentYolo;
      const desired = currentAuto ? "autonomous" : currentYolo ? "code" : "ask";
      try {
        await applyMode(desired);
      } catch (err) {
        console.error(formatUserError(err, "Failed to set mode"));
      }
      continue;
    }

    if (trimmed === "/autonomous") {
      currentAuto = !currentAuto;
      const desired = currentAuto ? "autonomous" : currentYolo ? "code" : "ask";
      try {
        await applyMode(desired);
      } catch (err) {
        console.error(formatUserError(err, "Failed to set mode"));
      }
      continue;
    }

    if (trimmed === "/plan") {
      currentYolo = false;
      currentAuto = false;
      try {
        const applied = await client.setMode(session.sessionId, "plan", 60_000);
        if (applied) {
          console.log(chalk.dim("Switched to plan mode."));
        } else {
          console.log(chalk.yellow("Plan mode is not available on this agent."));
        }
      } catch (err) {
        console.error(formatUserError(err, "Failed to set plan mode"));
      }
      continue;
    }

    if (trimmed.startsWith("/loop")) {
      const rest = trimmed.slice("/loop".length).trim();
      let count = 3;
      let promptText = rest;
      const match = rest.match(/^(\d+)(?:\s+(.*))?$/s);
      if (match) {
        count = Math.max(1, Math.min(20, parseInt(match[1], 10)));
        promptText = match[2]?.trim() ?? "";
      }
      if (!promptText) {
        console.log(chalk.yellow("Usage: /loop [count] <prompt>"));
        continue;
      }
      try {
        for (let i = 0; i < count; i++) {
          const text = await buildLoopPrompt(cwd, promptText, i, count);
          const turnDone = new Promise<void>((resolve, reject) => {
            turnResolver = resolve;
            turnRejecter = reject;
          });
          turnDone.catch(() => {});
          const turnTimeout = setTimeout(() => {
            turnRejecter?.(new Error("Timed out waiting for agent turn completion"));
          }, TURN_TIMEOUT_MS);
          try {
            await client.prompt(session.sessionId, [{ type: "text", text }]);
            await turnDone;
          } finally {
            clearTimeout(turnTimeout);
            turnResolver = undefined;
            turnRejecter = undefined;
          }
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        if (isRateLimited(message)) {
          console.error(chalk.yellow(formatRateLimitMessage()));
        } else {
          console.error(chalk.red(`Loop failed: ${message}`));
        }
      }
      continue;
    }

    if (trimmed.startsWith("/swarm")) {
      const promptText = trimmed.slice("/swarm".length).trim();
      if (!promptText) {
        console.log(chalk.yellow("Usage: /swarm <prompt>"));
      } else {
        try {
          await swarmCommand({
            prompt: promptText,
            cwd,
            yolo: currentYolo,
            model: currentModel,
            maxTurns: 10,
          });
        } catch (err) {
          console.error(formatUserError(err, "Swarm failed"));
        }
      }
      continue;
    }

    if (trimmed.startsWith("/schedule")) {
      const rest = trimmed.slice("/schedule".length).trim();
      const parts = rest.split(/\s+/);
      const sub = parts[0]?.toLowerCase() ?? "";
      const name = parts[1];
      try {
        switch (sub) {
          case "":
          case "list":
            await scheduleListCommand();
            break;
          case "start":
            await scheduleStartCommand();
            break;
          case "stop-daemon":
            await scheduleStopDaemonCommand();
            break;
          case "stop":
            if (!name) {
              console.log(chalk.yellow("Usage: /schedule stop <name>"));
              continue;
            }
            await scheduleStopCommand(name);
            break;
          case "run":
            if (!name) {
              console.log(chalk.yellow("Usage: /schedule run <name>"));
              continue;
            }
            await scheduleRunCommand(name);
            break;
          case "delete":
            if (!name) {
              console.log(chalk.yellow("Usage: /schedule delete <name>"));
              continue;
            }
            await scheduleDeleteCommand(name);
            break;
          default:
            console.log(chalk.yellow("Usage: /schedule [list|start|stop-daemon|stop <name>|run <name>|delete <name>]"));
        }
      } catch (err) {
        console.error(formatUserError(err, "Schedule command failed"));
      }
      continue;
    }

    if (trimmed.startsWith("/btw")) {
      const note = trimmed.slice("/btw".length).trim();
      const prompt = note
        ? `[Side note / aside] ${note}\n\nThis is an off-topic aside. Do not run commands or edit files. Just reply briefly and helpfully.`
        : "[Side note / aside] What's on your mind? This is an off-topic chat; do not run commands or edit files, just reply briefly.";
      try {
        await client.prompt(session.sessionId, [{ type: "text", text: prompt }]);
      } catch (err) {
        console.error(formatUserError(err, "Side note failed"));
      }
      continue;
    }

    try {
      await client.prompt(session.sessionId, [{ type: "text", text: line }]);
    } catch (err) {
      console.error(formatUserError(err, "Prompt failed"));
    }
  }
}

function renderUpdate(update: AcpUpdate): void {
  switch (update.sessionUpdate) {
    case "agent_message_chunk": {
      const text = (update.content as { type: string; text: string } | undefined)?.text ?? "";
      process.stdout.write(text);
      break;
    }
    case "agent_thought_chunk":
      // Suppress thinking chunks in CLI by default.
      break;
    case "tool_call":
      console.log(chalk.dim(`\n[tool: ${update.title ?? "unknown"}]`));
      break;
    case "tool_call_update":
      console.log(chalk.dim(`[tool update: ${update.status ?? update.title ?? ""}]`));
      break;
    case "turn_completed":
      process.stdout.write("\n\n");
      break;
    default:
      // Ignore unknown updates.
      break;
  }
}

async function handlePermission(req: AcpPermissionRequest, rl: readline.Interface): Promise<AcpPermissionResponse> {
  console.log(chalk.yellow(`\nPermission requested: ${req.toolCall.title ?? req.toolCall.command ?? "tool call"}`));
  req.options.forEach((opt, i) => {
    console.log(`  ${i + 1}. ${opt.name}${opt.kind ? ` (${opt.kind})` : ""}`);
  });
  console.log("  0. Cancel");
  const answer = await rl.question("Choose option: ");
  const index = parseInt(answer.trim(), 10) - 1;
  if (Number.isNaN(index) || index < 0 || index >= req.options.length) {
    return { outcome: { outcome: "cancelled" } };
  }
  return { outcome: { outcome: "selected", optionId: req.options[index].optionId } };
}
