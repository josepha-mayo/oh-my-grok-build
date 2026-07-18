import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { AcpClient } from "./acp.js";

class FakeWebSocket {
  static last: FakeWebSocket | null = null;
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSING = 2;
  static CLOSED = 3;
  url: string;
  sent: unknown[] = [];
  readyState = FakeWebSocket.CONNECTING;
  private listeners: Map<string, Set<(ev: Event) => void>> = new Map();

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.last = this;
    // Defer open so listeners can be attached.
    setTimeout(() => this.open(), 0);
  }

  addEventListener(event: string, handler: (ev: Event) => void) {
    const set = this.listeners.get(event) ?? new Set();
    set.add(handler);
    this.listeners.set(event, set);
  }

  removeEventListener(event: string, handler: (ev: Event) => void) {
    this.listeners.get(event)?.delete(handler);
  }

  private emit(event: string, ev: Event) {
    for (const handler of this.listeners.get(event) ?? []) {
      handler(ev);
    }
  }

  open() {
    this.readyState = 1;
    this.emit("open", new Event("open"));
  }

  send(data: unknown) {
    this.sent.push(typeof data === "string" ? JSON.parse(data) : data);
  }

  receive(data: unknown) {
    this.emit(
      "message",
      new MessageEvent("message", { data: JSON.stringify(data) }),
    );
  }

  close() {
    this.readyState = 3;
    this.emit("close", new Event("close"));
  }
}

describe("AcpClient", () => {
  beforeEach(() => {
    vi.stubGlobal("WebSocket", FakeWebSocket);
    FakeWebSocket.last = null;
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function makeClient(yolo = false) {
    return new AcpClient({
      url: "ws://localhost:7331/ws?server-key=abc",
      yolo,
      handlers: {
        onUpdate: vi.fn(),
        onError: vi.fn(),
        onClose: vi.fn(),
        onAskUser: vi.fn().mockResolvedValue("ok"),
      },
    });
  }

  it("connects and resolves ready when the socket opens", async () => {
    const client = makeClient();
    await client.ready();
    expect(FakeWebSocket.last?.url).toBe(
      "ws://localhost:7331/ws?server-key=abc",
    );
  });

  it("initializes and reports authentication methods", async () => {
    const client = makeClient();
    await client.ready();
    const initPromise = client.initialize(
      { terminal: true, fs: { readTextFile: true, writeTextFile: true } },
      5_000,
    );

    const ws = FakeWebSocket.last!;
    const request = ws.sent[ws.sent.length - 1] as Record<string, unknown>;
    expect(request.method).toBe("initialize");

    ws.receive({
      jsonrpc: "2.0",
      id: request.id,
      result: { authMethods: [{ id: "xai.api_key" }] },
    });

    const result = await initPromise;
    expect(result.authMethods).toEqual([{ id: "xai.api_key" }]);
  });

  it("authenticates with the selected method", async () => {
    const client = makeClient();
    await client.ready();
    const initPromise = client.initialize({ terminal: true }, 5_000);
    const ws = FakeWebSocket.last!;
    const initRequest = ws.sent.at(-1) as Record<string, unknown>;
    ws.receive({
      jsonrpc: "2.0",
      id: initRequest.id,
      result: { authMethods: [{ id: "xai.api_key" }] },
    });
    await initPromise;

    const authPromise = client.authenticate("xai.api_key");
    const authRequest = ws.sent.at(-1) as Record<string, unknown>;
    expect(authRequest.method).toBe("authenticate");
    ws.receive({ jsonrpc: "2.0", id: authRequest.id, result: { ok: true } });
    await authPromise;
  });

  it("creates a new session and sets mode", async () => {
    const client = makeClient(true);
    await client.ready();

    const initPromise = client.initialize({ terminal: true }, 5_000);
    const ws = FakeWebSocket.last!;
    const initRequest = ws.sent.at(-1) as Record<string, unknown>;
    ws.receive({
      jsonrpc: "2.0",
      id: initRequest.id,
      result: { authMethods: [] },
    });
    await initPromise;

    const newSessionPromise = client.newSession("/repo");
    const newSessionRequest = ws.sent.at(-1) as Record<string, unknown>;
    expect(newSessionRequest.method).toBe("session/new");
    ws.receive({
      jsonrpc: "2.0",
      id: newSessionRequest.id,
      result: {
        sessionId: "session-123",
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
      },
    });
    const session = await newSessionPromise;
    expect(session.sessionId).toBe("session-123");

    const modePromise = client.setMode(session.sessionId, "code");
    const modeRequest = ws.sent.at(-1) as Record<string, unknown>;
    expect(modeRequest.method).toBe("session/set_config_option");
    ws.receive({
      jsonrpc: "2.0",
      id: modeRequest.id,
      result: { ok: true },
    });
    const ok = await modePromise;
    expect(ok).toBe(true);
  });

  it("resolves a prompt when turn_completed arrives", async () => {
    const client = makeClient();
    await client.ready();

    const initPromise = client.initialize({ terminal: true }, 5_000);
    const ws = FakeWebSocket.last!;
    const initRequest = ws.sent.at(-1) as Record<string, unknown>;
    ws.receive({
      jsonrpc: "2.0",
      id: initRequest.id,
      result: { authMethods: [] },
    });
    await initPromise;

    const promptPromise = client.prompt("session-123", [
      { type: "text", text: "hello" },
    ]);
    const promptRequest = ws.sent.at(-1) as Record<string, unknown>;
    expect(promptRequest.method).toBe("session/prompt");

    ws.receive({
      jsonrpc: "2.0",
      method: "session/update",
      params: {
        sessionId: "session-123",
        update: { sessionUpdate: "turn_completed" },
      },
    });

    await promptPromise;
  });

  it("runs a terminal command via x.ai/run_terminal_cmd", async () => {
    const client = makeClient();
    await client.ready();

    const promise = client.runTerminalCmd(["schedule", "list"]);
    const ws = FakeWebSocket.last!;
    const request = ws.sent.at(-1) as Record<string, unknown>;
    expect(request.method).toBe("x.ai/run_terminal_cmd");
    expect(request.params).toEqual({ args: ["schedule", "list"] });

    ws.receive({
      jsonrpc: "2.0",
      id: request.id,
      result: { output: "cron job\n", exitCode: 0 },
    });

    const result = await promise;
    expect(result).toEqual({ output: "cron job\n", exitCode: 0 });
  });

  it("auto-approves permissions in yolo mode", async () => {
    const client = makeClient(true);
    await client.ready();

    const ws = FakeWebSocket.last!;
    ws.receive({
      jsonrpc: "2.0",
      id: 7,
      method: "session/request_permission",
      params: {
        sessionId: "session-123",
        options: [
          { optionId: "cancel", name: "Cancel", kind: "cancel" },
          { optionId: "allow_once", name: "Allow once", kind: "allow_once" },
          {
            optionId: "allow_always",
            name: "Always allow",
            kind: "allow_always",
          },
        ],
      },
    });

    // Give the microtask queue a chance to send the response.
    await new Promise((resolve) => setTimeout(resolve, 0));

    const response = ws.sent.at(-1) as Record<string, unknown>;
    expect(response.id).toBe(7);
    expect((response.result as any)?.outcome?.optionId).toBe("allow_always");
  });

  it("selects allow_once by default for permissions", async () => {
    const client = makeClient(false);
    await client.ready();

    const ws = FakeWebSocket.last!;
    ws.receive({
      jsonrpc: "2.0",
      id: 8,
      method: "session/request_permission",
      params: {
        sessionId: "session-123",
        options: [
          { optionId: "cancel", name: "Cancel", kind: "cancel" },
          { optionId: "allow_once", name: "Allow once", kind: "allow_once" },
        ],
      },
    });

    await new Promise((resolve) => setTimeout(resolve, 0));

    const response = ws.sent.at(-1) as Record<string, unknown>;
    expect(response.id).toBe(8);
    expect((response.result as any)?.outcome?.optionId).toBe("allow_once");
  });
});
