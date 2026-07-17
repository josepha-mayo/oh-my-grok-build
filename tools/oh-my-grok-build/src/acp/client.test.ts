import { describe, it, beforeEach } from "node:test";
import assert from "node:assert";
import { AcpClient, type AcpPermissionRequest, type AcpTransport } from "./client.js";

class FakeSocket {
  readyState = 1; // OPEN
  onopen: (() => void) | null = null;
  onclose: ((ev?: { code: number; reason: string }) => void) | null = null;
  onerror: ((err: Error) => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  sent: string[] = [];

  send(data: string) {
    this.sent.push(data);
  }

  close() {
    this.readyState = 3;
    this.onclose?.({ code: 1000, reason: "" });
  }

  receive(data: string) {
    this.onmessage?.({ data });
  }

  lastSent(): string | undefined {
    return this.sent.at(-1);
  }
}

function makeTransport(socket: FakeSocket): AcpTransport {
  const transport: AcpTransport = {
    send: (m) => socket.send(m),
    close: () => socket.close(),
    onMessage: undefined,
  };
  socket.onmessage = (ev) => transport.onMessage?.(ev.data);
  return transport;
}

describe("AcpClient", () => {
  let socket: FakeSocket;
  let transport: AcpTransport;

  beforeEach(() => {
    socket = new FakeSocket();
    transport = makeTransport(socket);
  });

  it("sends initialize and resolves response", async () => {
    const client = new AcpClient(transport);
    const initPromise = client.initialize(1, { terminal: true });
    socket.receive(JSON.stringify({ jsonrpc: "2.0", id: 1, result: { success: true } }));
    const result = await initPromise;
    assert.deepStrictEqual(result, { success: true });
    const sent = JSON.parse(socket.lastSent()!);
    assert.strictEqual(sent.method, "initialize");
  });

  it("handles session/update notifications", async () => {
    const updates: { sessionId: string; update: unknown }[] = [];
    const client = new AcpClient(transport, {
      onUpdate: (sid, update) => updates.push({ sessionId: sid, update }),
    });
    socket.receive(
      JSON.stringify({
        jsonrpc: "2.0",
        method: "session/update",
        params: { sessionId: "s1", update: { sessionUpdate: "agent_message_chunk", content: { type: "text", text: "hi" } } },
      })
    );
    assert.strictEqual(updates.length, 1);
    assert.strictEqual(updates[0].sessionId, "s1");
  });

  it("responds to session/request_permission", async () => {
    let captured: AcpPermissionRequest | undefined;
    const client = new AcpClient(transport, {
      onPermission: async (req) => {
        captured = req;
        return { outcome: { outcome: "selected", optionId: "allow" } };
      },
    });
    const request = {
      jsonrpc: "2.0",
      id: 5,
      method: "session/request_permission",
      params: { sessionId: "s1", toolCall: { toolCallId: "tc1", title: "Run ls" }, options: [{ optionId: "allow", name: "Allow" }] },
    };
    socket.receive(JSON.stringify(request));
    await new Promise((r) => setTimeout(r, 10));
    assert.ok(captured);
    assert.strictEqual(captured!.toolCall.title, "Run ls");
    const sent = JSON.parse(socket.lastSent()!);
    assert.strictEqual(sent.id, 5);
    assert.deepStrictEqual(sent.result, { outcome: { outcome: "selected", optionId: "allow" } });
  });

  it("times out if no response", async () => {
    const client = new AcpClient(transport);
    await assert.rejects(client.initialize(1, {}, 100), /timed out/);
  });
});
