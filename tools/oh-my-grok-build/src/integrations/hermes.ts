import path from "node:path";
import { AcpClient } from "../acp/client.js";
import { createStdioTransport } from "../acp/stdio.js";
import type { Connector, ConnectorConfig, ConnectorResult } from "./types.js";

const INTERACTIVE_AUTH_IDS = new Set(["hermes-setup", "oauth", "browser", "grok.com"]);

function selectAuthMethod(methods: { id: string }[] | undefined): { id: string } | undefined {
  return methods?.find((m) => !INTERACTIVE_AUTH_IDS.has(m.id.toLowerCase())) ?? methods?.[0];
}

export class HermesConnector implements Connector {
  private client?: AcpClient;
  constructor(readonly config: ConnectorConfig) {}

  async run(prompt: string): Promise<ConnectorResult> {
    const command = this.config.command ?? "hermes";
    const args = ["acp"];
    const env: Record<string, string> = {};
    for (const [k, v] of Object.entries({ ...process.env, ...this.config.env })) {
      if (v !== undefined) env[k] = v;
    }

    const transport = createStdioTransport({
      command,
      args,
      cwd: this.config.cwd ?? process.cwd(),
      env,
    });

    const chunks: string[] = [];
    let turnResolver: (() => void) | undefined;
    let turnRejecter: ((err: Error) => void) | undefined;
    const turnDone = new Promise<void>((resolve, reject) => {
      turnResolver = resolve;
      turnRejecter = reject;
    });
    const timeout = setTimeout(() => {
      turnRejecter?.(new Error("Hermes prompt timed out"));
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
        const option = req.options.find((o) => o.kind === "allow_once") ?? req.options[0];
        return { outcome: option ? { outcome: "selected", optionId: option.optionId } : { outcome: "cancelled" } };
      },
      onError: (err) => {
        turnRejecter?.(err);
      },
    });
    this.client = client;

    try {
      const init = await client.initialize(
        1,
        { terminal: true, fs: { readTextFile: true, writeTextFile: true } },
        30_000
      );
      const authMethod = selectAuthMethod(init.authMethods);
      if (authMethod && !INTERACTIVE_AUTH_IDS.has(authMethod.id.toLowerCase())) {
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
