import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import { useCommand, type UseOptions } from "./use.js";
import { setupOmgHome, cleanupOmgHome } from "../test-utils.js";
import type { AcpTransport } from "../acp/client.js";
import type { AcpMessage } from "../types.js";

class FakeAcpTransport implements AcpTransport {
  sent: AcpMessage[] = [];
  onMessage?: (message: string) => void;
  onOpen?: () => void;
  onClose?: (code?: number, reason?: string) => void;
  onError?: (err: Error) => void;

  send(message: string): void {
    const msg = JSON.parse(message) as AcpMessage;
    this.sent.push(msg);
    this.respond(msg);
  }

  close(): void {}

  private respond(req: AcpMessage): void {
    const id = req.id;
    const method = req.method;
    if (method === "initialize") {
      this.reply(id, {
        authMethods: [{ id: "xai.api_key" }],
        configOptions: [
          {
            id: "mode",
            category: "mode",
            name: "Mode",
            options: [
              { value: "ask", name: "Ask" },
              { value: "code", name: "Code" },
            ],
            currentValue: "ask",
          },
        ],
      });
      return;
    }
    if (method === "authenticate") {
      this.reply(id, { ok: true });
      return;
    }
    if (method === "session/new") {
      this.reply(id, {
        sessionId: "sess-1",
        configOptions: [
          {
            id: "mode",
            category: "mode",
            name: "Mode",
            options: [
              { value: "ask", name: "Ask" },
              { value: "code", name: "Code" },
            ],
            currentValue: "ask",
          },
        ],
      });
      return;
    }
    if (method === "session/set_config_option") {
      this.reply(id, { ok: true });
      return;
    }
    if (method === "session/prompt") {
      this.reply(id, { ok: true });
      // Trigger turn completion so useCommand can finish.
      this.notify("session/update", {
        sessionId: "sess-1",
        update: { sessionUpdate: "turn_completed" },
      });
      return;
    }
  }

  private reply(id: string | number | null | undefined, result: unknown): void {
    this.onMessage?.(JSON.stringify({ jsonrpc: "2.0", id, result }));
  }

  private notify(method: string, params: unknown): void {
    this.onMessage?.(JSON.stringify({ jsonrpc: "2.0", method, params }));
  }
}

describe("use command", () => {
  let tempDir: string;

  beforeEach(() => {
    tempDir = setupOmgHome();
  });

  afterEach(() => {
    cleanupOmgHome(tempDir);
  });

  it("runs a browser use session with the omgb-browser MCP server", async () => {
    const transport = new FakeAcpTransport();
    const result = await useCommand({
      prompt: "go to example.com and summarize",
      mode: "browser",
      transport,
    });

    assert.equal(typeof result, "string");

    const init = transport.sent.find((m) => m.method === "initialize");
    assert.ok(init);

    const newSession = transport.sent.find((m) => m.method === "session/new");
    assert.ok(newSession);
    const mcpServers = (newSession?.params as any)?.mcpServers ?? [];
    assert.ok(
      mcpServers.some((s: any) => s.name === "omgb-browser"),
      "expected omgb-browser MCP server in session/new"
    );
    assert.ok(!mcpServers.some((s: any) => s.name === "omgb-computer"), "did not expect omgb-computer in browser mode");

    const prompt = transport.sent.find((m) => m.method === "session/prompt");
    assert.ok(prompt);
    assert.equal((prompt?.params as any)?.prompt?.[0]?.text, "go to example.com and summarize");
  });

  it("runs a computer use session with omgb-computer + omgb-browser and enables desktop control", async () => {
    const transport = new FakeAcpTransport();
    await useCommand({
      prompt: "open calculator",
      mode: "computer",
      transport,
    });

    const newSession = transport.sent.find((m) => m.method === "session/new");
    assert.ok(newSession);
    const mcpServers = (newSession?.params as any)?.mcpServers ?? [];
    assert.ok(
      mcpServers.some((s: any) => s.name === "omgb-computer"),
      "expected omgb-computer MCP server in computer mode"
    );
    assert.ok(
      mcpServers.some((s: any) => s.name === "omgb-browser"),
      "expected omgb-browser MCP server in computer mode"
    );
  });

  it("passes --yolo to the agent when requested", async () => {
    const transport = new FakeAcpTransport();
    await useCommand({
      prompt: "do it",
      mode: "browser",
      yolo: true,
      transport,
    });

    const setConfig = transport.sent.find((m) => m.method === "session/set_config_option");
    assert.ok(setConfig);
    assert.equal((setConfig?.params as any)?.value, "code");
  });
});
