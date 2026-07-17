import readline from "node:readline/promises";
import chalk from "chalk";
import { AcpClient } from "../acp/client.js";
import { createNodeWebSocketTransport } from "../acp/transport.js";
import { parseServerUrl } from "../acp/server.js";
import type { AcpPermissionRequest, AcpUpdate } from "../types.js";

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

  const headers = { Authorization: `Bearer ${parsed.secret}` };
  const transport = await createNodeWebSocketTransport(options.url, headers);

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

  await client.initialize(
    1,
    {
      terminal: true,
      fs: { readTextFile: true, writeTextFile: false },
    },
    30_000
  );

  const cwd = options.cwd ?? process.cwd();
  const session = await client.newSession(
    cwd,
    [],
    { yoloMode: options.yolo ?? false, modelId: options.model },
    60_000
  );
  console.log(chalk.dim(`Session: ${session.sessionId}\n`));

  while (true) {
    const line = await rl.question(chalk.bold("you> "));
    if (!line.trim()) continue;
    if (line.trim() === "/quit" || line.trim() === "/exit") {
      client.close();
      break;
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

async function handlePermission(req: AcpPermissionRequest, rl: readline.Interface): Promise<{ outcome: { outcome: "selected"; optionId: string } }> {
  console.log(chalk.yellow(`\nPermission requested: ${req.toolCall.title ?? req.toolCall.command ?? "tool call"}`));
  req.options.forEach((opt, i) => {
    console.log(`  ${i + 1}. ${opt.name}${opt.kind ? ` (${opt.kind})` : ""}`);
  });
  const answer = await rl.question("Choose option: ");
  const index = parseInt(answer.trim(), 10) - 1;
  const optionId = req.options[index]?.optionId ?? req.options[0]?.optionId ?? "cancel";
  return { outcome: { outcome: "selected", optionId } };
}
