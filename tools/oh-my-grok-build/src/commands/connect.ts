import readline from "node:readline/promises";
import chalk from "chalk";
import { AcpClient } from "../acp/client.js";
import { createNodeWebSocketTransport } from "../acp/transport.js";
import { parseServerUrl } from "../acp/server.js";
import type { AcpPermissionRequest, AcpPermissionResponse, AcpUpdate } from "../types.js";

export interface ConnectOptions {
  url: string;
  cwd?: string;
  yolo?: boolean;
  model?: string;
}

export async function connectCommand(options: ConnectOptions): Promise<void> {
  const parsed = parseServerUrl(options.url);
  if (!parsed.secret) {
    throw new Error("URL must include a server-key query parameter, e.g. ws://host:port/ws?server-key=XYZ");
  }

  const transport = await createNodeWebSocketTransport(options.url, {});

  const rl = readline.createInterface({
    input: process.stdin,
    output: process.stdout,
  });

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
      process.exit(0);
    },
  });

  const init = await client.initialize(
    1,
    {
      terminal: true,
      fs: { readTextFile: true, writeTextFile: true },
    },
    30_000
  );

  const authMethod = init.authMethods?.find((m) => m.id === "xai.api_key") ?? init.authMethods?.[0];
  if (authMethod) {
    await client.authenticate(authMethod, 60_000);
  }

  const cwd = options.cwd ?? process.cwd();
  const session = await client.newSession(cwd, [], { yoloMode: options.yolo ?? false, modelId: options.model }, 60_000);
  console.log(chalk.dim(`Session: ${session.sessionId}\n`));

  while (true) {
    const line = await rl.question(chalk.bold("you> "));
    if (!line.trim()) continue;
    const trimmed = line.trim();
    if (trimmed === "/quit" || trimmed === "/exit") {
      client.close();
      break;
    }
    if (trimmed.startsWith("/model ")) {
      const modelId = trimmed.slice("/model ".length).trim();
      if (modelId) {
        try {
          await client.setModel(session.sessionId, modelId);
          console.log(chalk.dim(`Model set to ${modelId}`));
        } catch (err) {
          console.error(chalk.red(`Failed to set model: ${err instanceof Error ? err.message : String(err)}`));
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
