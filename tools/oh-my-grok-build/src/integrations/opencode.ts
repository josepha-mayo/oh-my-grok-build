import { AcpClient } from "../acp/client.js";
import { createNodeWebSocketTransport } from "../acp/transport.js";
import type { Connector, ConnectorConfig, ConnectorResult } from "./types.js";

export class OpenCodeConnector implements Connector {
  private client?: AcpClient;
  constructor(readonly config: ConnectorConfig) {}

  async run(prompt: string): Promise<ConnectorResult> {
    const url = this.config.url ?? "ws://127.0.0.1:7331/acp";
    const headers: Record<string, string> = {};
    if (this.config.secret) headers.Authorization = `Bearer ${this.config.secret}`;
    const transport = await createNodeWebSocketTransport(url, headers);

    const chunks: string[] = [];
    const client = new AcpClient(transport, {
      onUpdate: (_sid, update) => {
        if (update.sessionUpdate === "agent_message_chunk") {
          const text = (update.content as { text?: string } | undefined)?.text ?? "";
          chunks.push(text);
        }
      },
      onPermission: async (req) => {
        // Default to the first option; in production delegate to user.
        return { outcome: { outcome: "selected", optionId: req.options[0]?.optionId ?? "allow" } };
      },
    });

    await client.initialize(1, { terminal: true, fs: { readTextFile: true, writeTextFile: false } }, 30_000);
    const { sessionId } = await client.newSession(this.config.cwd ?? process.cwd(), [], { yoloMode: true }, 60_000);
    await client.prompt(sessionId, [{ type: "text", text: prompt }], 600_000);
    client.close();

    return { text: chunks.join("") };
  }

  async close(): Promise<void> {
    this.client?.close();
  }
}
