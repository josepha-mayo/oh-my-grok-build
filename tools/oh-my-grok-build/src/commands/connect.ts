import path from "node:path";
import readline from "node:readline/promises";
import type { Readable as ReadableStream, Writable as WritableStream } from "node:stream";
import chalk from "chalk";
import { AcpClient } from "../acp/client.js";
import { createNodeWebSocketTransport } from "../acp/transport.js";
import { parseServerUrl } from "../acp/server.js";
import { isAllowedWsUrl } from "../net.js";
import { swarmCommand } from "./swarm.js";
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
  "/new",
  "/clear",
  "/quit",
  "/exit",
];

const EFFORTS: ReasoningEffort[] = ["low", "medium", "high", "max"];

export async function connectCommand(options: ConnectOptions): Promise<void> {
  const allowed = isAllowedWsUrl(options.url);
  if (!allowed.ok) {
    throw new Error(`Cannot connect: ${allowed.reason}`);
  }

  const parsed = parseServerUrl(options.url);
  if (!parsed.secret) {
    throw new Error("URL must include a server-key query parameter, e.g. ws://host:port/ws?server-key=XYZ");
  }

  const transport = await createNodeWebSocketTransport(parsed.baseUrl, {
    Authorization: `Bearer ${parsed.secret}`,
  });

  const input = options.input ?? process.stdin;
  const output = options.output ?? process.stdout;
  const exit = options.exit ?? ((code: number) => process.exit(code));

  const rl = readline.createInterface({ input, output });

  const client = new AcpClient(transport, {
    onUpdate: (_sessionId, update) => renderUpdate(update),
    onPermission: async (req) => handlePermission(req, rl),
    onAskUser: async ({ question }) => {
      console.log(chalk.yellow(`\n${question}`));
      return rl.question("Your answer: ");
    },
    onError: (err) => console.error(chalk.red(`ACP error: ${err.message}`)),
    onClose: () => {
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
  let currentModel = options.model ?? "grok-build";
  let currentEffort: ReasoningEffort = "medium";
  let currentYolo = options.yolo ?? false;
  let currentAuto = false;

  function buildSessionMeta(): Record<string, unknown> {
    const meta: Record<string, unknown> = {
      yoloMode: currentYolo,
      autoMode: currentAuto,
      reasoningEffort: currentEffort,
    };
    if (currentModel) meta.modelId = currentModel;
    return meta;
  }

  async function startSession(): Promise<AcpNewSessionResponse> {
    const mcpServers = toAcpMcpServers(await loadMcpConfig());
    const session = await client.newSession(cwd, mcpServers, buildSessionMeta(), 60_000);
    console.log(chalk.dim(`Session: ${session.sessionId}\n`));
    return session;
  }

  let session = await startSession();

  async function applyMode(desired: string): Promise<void> {
    const applied = await client.setMode(session.sessionId, desired, 60_000).catch(() => false);
    if (applied) {
      console.log(chalk.dim(`Mode set to ${desired}.`));
      return;
    }
    session = await startSession();
    console.log(chalk.dim(`Started new session in ${desired} mode.`));
  }

  async function applyEffort(): Promise<void> {
    await client.setModelWithEffort(session.sessionId, currentModel, currentEffort, 60_000);
    console.log(chalk.dim(`Reasoning effort set to ${currentEffort}.`));
  }

  async function applyModel(modelId: string): Promise<void> {
    await client.setModelWithEffort(session.sessionId, modelId, currentEffort, 60_000);
    currentModel = modelId;
    console.log(chalk.dim(`Model set to ${modelId}.`));
  }

  while (true) {
    const line = await rl.question(chalk.bold("you> "));
    if (!line.trim()) continue;
    const trimmed = line.trim();

    if (trimmed === "/help" || trimmed === "/?") {
      console.log(chalk.dim("Available commands:"));
      SLASH_COMMANDS.forEach((c) => console.log(chalk.dim(`  ${c}`)));
      continue;
    }

    if (trimmed === "/quit" || trimmed === "/exit") {
      client.close();
      break;
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
          console.error(chalk.red(`Failed to set model: ${err instanceof Error ? err.message : String(err)}`));
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
          console.error(chalk.red(`Failed to set effort: ${err instanceof Error ? err.message : String(err)}`));
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
        console.error(chalk.red(`Failed to set mode: ${err instanceof Error ? err.message : String(err)}`));
      }
      continue;
    }

    if (trimmed === "/autonomous") {
      currentAuto = !currentAuto;
      const desired = currentAuto ? "autonomous" : currentYolo ? "code" : "ask";
      try {
        await applyMode(desired);
      } catch (err) {
        console.error(chalk.red(`Failed to set mode: ${err instanceof Error ? err.message : String(err)}`));
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
        console.error(chalk.red(`Failed to set plan mode: ${err instanceof Error ? err.message : String(err)}`));
      }
      continue;
    }

    if (trimmed.startsWith("/loop")) {
      const rest = trimmed.slice("/loop".length).trim();
      let count = 3;
      let promptText = rest;
      const match = rest.match(/^(\d+)\s+(.*)$/s);
      if (match) {
        count = Math.max(1, Math.min(20, parseInt(match[1], 10)));
        promptText = match[2];
      }
      if (!promptText) {
        console.log(chalk.yellow("Usage: /loop [count] <prompt>"));
        continue;
      }
      try {
        for (let i = 0; i < count; i++) {
          const text =
            i === 0
              ? promptText
              : i === count - 1
                ? `Wrap up and finalize. Original task: ${promptText}`
                : `Review the result above, fix any issues, and continue. Original task: ${promptText}`;
          await client.prompt(session.sessionId, [{ type: "text", text }]);
        }
      } catch (err) {
        console.error(chalk.red(`Loop failed: ${err instanceof Error ? err.message : String(err)}`));
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
          console.error(chalk.red(`Swarm failed: ${err instanceof Error ? err.message : String(err)}`));
        }
      }
      continue;
    }

    try {
      await client.prompt(session.sessionId, [{ type: "text", text: line }]);
    } catch (err) {
      console.error(chalk.red(`Prompt failed: ${err instanceof Error ? err.message : String(err)}`));
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
