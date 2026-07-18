import path from "node:path";
import { AcpClient } from "../acp/client.js";
import { createNodeWebSocketTransport } from "../acp/transport.js";
import { isAllowedWsUrl } from "../net.js";
import type { Connector, ConnectorConfig, ConnectorResult } from "./types.js";

function selectPermissionOption(options: { optionId: string; kind?: string }[]): string {
  const allowOnce = options.find((o) => o.kind === "allow_once" || /allow.once/i.test(o.optionId));
  if (allowOnce) return allowOnce.optionId;
  return options[0]?.optionId ?? "";
}

export class OpenCodeConnector implements Connector {
  private client?: AcpClient;
  constructor(readonly config: ConnectorConfig) {}

  async run(prompt: string): Promise<ConnectorResult> {
    const rawUrl = this.config.url ?? "ws://127.0.0.1:7331/acp";
    const allowed = isAllowedWsUrl(rawUrl);
    if (!allowed.ok) throw new Error(allowed.reason);
    const urlObj = new URL(rawUrl);
    const secret = this.config.secret ?? urlObj.searchParams.get("server-key") ?? undefined;
    urlObj.searchParams.delete("server-key");
    const url = urlObj.toString();
    const headers: Record<string, string> = {};
    if (secret) headers.Authorization = `Bearer ${secret}`;
    const transport = await createNodeWebSocketTransport(url, headers);

    const chunks: string[] = [];
    let turnResolver: (() => void) | undefined;
    let turnRejecter: ((err: Error) => void) | undefined;
    const turnDone = new Promise<void>((resolve, reject) => {
      turnResolver = resolve;
      turnRejecter = reject;
    });
    const timeout = setTimeout(() => {
      turnRejecter?.(new Error("OpenCode prompt timed out"));
    }, 600_000);

    const client = new AcpClient(transport, {
      onUpdate: (_sid, update) => {
        if (update.sessionUpdate === "agent_message_chunk") {
          const text = (update.content as { text?: string } | undefined)?.text ?? "";
          chunks.push(text);
        }
        if (update.sessionUpdate === "turn_completed" || update.sessionUpdate === "stop") {
          clearTimeout(timeout);
          turnResolver?.();
        }
      },
      onPermission: async (req) => {
        const optionId = selectPermissionOption(req.options);
        return { outcome: optionId ? { outcome: "selected", optionId } : { outcome: "cancelled" } };
      },
    });
    this.client = client;

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
      const { sessionId } = await client.newSession(path.resolve(this.config.cwd ?? process.cwd()), [], {}, 60_000);
      await client.prompt(sessionId, [{ type: "text", text: prompt }], 120_000);
      await turnDone;
      return { text: chunks.join("") };
    } finally {
      clearTimeout(timeout);
      client.close();
    }
  }

  async close(): Promise<void> {
    this.client?.close();
  }
}
