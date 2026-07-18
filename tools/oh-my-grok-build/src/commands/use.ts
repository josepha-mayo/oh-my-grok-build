import path from "node:path";
import chalk from "chalk";
import { AcpClient } from "../acp/client.js";
import { createStdioTransport } from "../acp/stdio.js";
import { loadOmgConfig } from "../config.js";
import { builtInMcpServer, toAcpMcpServers } from "../mcp/mcp-config.js";
import { appendTimelineEvent } from "../timeline.js";
import { selectPermissionOption } from "../permissions.js";
import { isRateLimited, formatRateLimitMessage } from "../rate-limit.js";
import type { AcpPermissionRequest, AcpPermissionResponse, AcpUpdate } from "../types.js";

export interface UseOptions {
  prompt: string;
  mode: "computer" | "browser";
  model?: string;
  yolo?: boolean;
  cwd?: string;
  maxTurns?: number;
  timeoutMs?: number;
}

const DEFAULT_TIMEOUT = 10 * 60 * 1000;

function handlePermission(req: AcpPermissionRequest, yolo: boolean): AcpPermissionResponse {
  const optionId = selectPermissionOption(req.options, yolo);
  const label = req.toolCall.title ?? req.toolCall.command ?? "tool call";
  if (optionId) {
    const optionName = req.options.find((o) => o.optionId === optionId)?.name ?? optionId;
    const modeMsg = yolo ? "" : " (auto-approving once per turn; use --yolo to always allow)";
    process.stderr.write(chalk.yellow(`Auto-approving ${label}: ${optionName}${modeMsg}\n`));
  } else {
    process.stderr.write(chalk.yellow(`No approval option for ${label}; cancelling\n`));
  }
  return { outcome: optionId ? { outcome: "selected", optionId } : { outcome: "cancelled" } };
}

export async function useCommand(options: UseOptions): Promise<string> {
  const cfg = await loadOmgConfig();
  const model = options.model ?? cfg.defaultModel ?? "grok-build";
  const cwd = path.resolve(options.cwd ?? process.cwd());
  const timeoutMs = Number.isNaN(options.timeoutMs) ? DEFAULT_TIMEOUT : (options.timeoutMs ?? DEFAULT_TIMEOUT);

  const desiredServers = options.mode === "computer" ? ["omgb-computer", "omgb-browser"] : ["omgb-browser"];
  const mcpServers = desiredServers
    .map((name) => builtInMcpServer(name))
    .filter((s): s is NonNullable<ReturnType<typeof builtInMcpServer>> => Boolean(s))
    .flatMap((s) => toAcpMcpServers([{ ...s, enabled: true }]));

  if (mcpServers.length === 0) {
    throw new Error(`No built-in MCP server available for ${options.mode} mode.`);
  }

  const args = ["agent", "stdio"];
  args.push("--model", model);
  if (options.yolo) args.push("--yolo");
  if (typeof options.maxTurns === "number" && !Number.isNaN(options.maxTurns) && options.maxTurns > 0) {
    args.push("--max-turns", String(options.maxTurns));
  }

  const childEnv: Record<string, string> = { GROK_DISABLE_AUTOUPDATER: "1" };
  if (options.mode === "computer") {
    childEnv.OMGB_ALLOW_DESKTOP_CONTROL = "1";
  }

  appendTimelineEvent({
    type: options.mode === "computer" ? "computer_use_start" : "browser_use_start",
    model,
    prompt: options.prompt,
    cwd,
  });

  const transport = createStdioTransport({
    command: "grok",
    args,
    cwd,
    env: childEnv,
  });

  try {
    const chunks: string[] = [];
    let turnResolver: (() => void) | undefined;
    let turnRejecter: ((err: Error) => void) | undefined;
    const turnDone = new Promise<void>((resolve, reject) => {
      turnResolver = resolve;
      turnRejecter = reject;
    });

    const timeout = setTimeout(() => {
      turnRejecter?.(new Error(`${options.mode} use timed out after ${timeoutMs / 1000}s`));
    }, timeoutMs);

    const client = new AcpClient(transport, {
      onUpdate: (_sid, update) => {
        if (update.sessionUpdate === "agent_message_chunk") {
          const text = (update.content as { text?: string } | undefined)?.text ?? "";
          chunks.push(text);
          process.stdout.write(text);
        }
        if (update.sessionUpdate === "turn_completed" || update.sessionUpdate === "stop") {
          clearTimeout(timeout);
          turnResolver?.();
        }
      },
      onPermission: async (req) => handlePermission(req, options.yolo ?? false),
      onError: (err) => {
        clearTimeout(timeout);
        turnRejecter?.(err);
      },
      onClose: () => {
        clearTimeout(timeout);
        turnResolver?.();
      },
    });

    try {
      const init = await client.initialize(
        1,
        { terminal: true, fs: { readTextFile: true, writeTextFile: true } },
        30_000
      );
      const authMethod = init.authMethods?.find((m) => m.id === "xai.api_key") ?? init.authMethods?.[0];
      if (authMethod) {
        await client.authenticate(authMethod, 60_000);
      }
      const { sessionId } = await client.newSession(cwd, mcpServers, {}, 60_000);
      await client.setMode(sessionId, options.yolo ? "code" : "ask", 60_000).catch(() => false);
      await client.prompt(sessionId, [{ type: "text", text: options.prompt }], timeoutMs);
      await turnDone;
      const result = chunks.join("");
      appendTimelineEvent({
        type: options.mode === "computer" ? "computer_use_stop" : "browser_use_stop",
        model,
        promptLength: options.prompt.length,
        resultLength: result.length,
      });
      return result;
    } finally {
      client.close();
    }
  } finally {
    transport.close();
  }
}

export async function browserCommand(options: Omit<UseOptions, "mode">): Promise<void> {
  try {
    const result = await useCommand({ ...options, mode: "browser" });
    if (!result.trim()) {
      console.log(chalk.dim("(no output)"));
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    if (isRateLimited(message)) {
      console.error(chalk.yellow(formatRateLimitMessage()));
    } else {
      console.error(chalk.red(message));
    }
    process.exitCode = 1;
  }
}

export async function computerCommand(options: Omit<UseOptions, "mode">): Promise<void> {
  const alreadyEnabled = process.env.OMGB_ALLOW_DESKTOP_CONTROL === "1";
  console.log(
    chalk.bold(
      `Running computer-use agent. This can control your desktop; review any tool prompts carefully.\n` +
        (alreadyEnabled
          ? "Desktop control is already enabled for this shell."
          : "Temporarily enabling desktop control for this run only.")
    )
  );
  try {
    const result = await useCommand({ ...options, mode: "computer" });
    if (!result.trim()) {
      console.log(chalk.dim("(no output)"));
    }
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    if (isRateLimited(message)) {
      console.error(chalk.yellow(formatRateLimitMessage()));
    } else {
      console.error(chalk.red(message));
    }
    process.exitCode = 1;
  }
}
