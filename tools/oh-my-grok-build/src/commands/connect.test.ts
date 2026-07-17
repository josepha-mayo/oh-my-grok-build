import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { PassThrough } from "node:stream";
import { WebSocketServer } from "ws";
import type { WebSocket } from "ws";
import { connectCommand } from "./connect.js";
import { setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;
let originalStdin: typeof process.stdin;
let originalExit: typeof process.exit;

function setProcessStdin(value: typeof process.stdin) {
  Object.defineProperty(process, "stdin", { value, configurable: true });
}

function setProcessExit(value: typeof process.exit) {
  Object.defineProperty(process, "exit", { value, configurable: true });
}

describe("connect command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
  });

  afterEach(() => {
    if (originalStdin) setProcessStdin(originalStdin);
    if (originalExit) setProcessExit(originalExit);
    cleanupOmgHome(tempDir);
  });

  it("connects to a server, initializes, and handles a /quit", async () => {
    const wss = new WebSocketServer({ host: "127.0.0.1", port: 0 });
    const urlBase = await new Promise<string>((resolve) => {
      wss.on("listening", () => {
        const addr = wss.address();
        resolve(`ws://127.0.0.1:${(addr as { port: number }).port}`);
      });
    });

    const received: string[] = [];
    wss.on("connection", (ws: WebSocket) => {
      ws.on("message", (data) => {
        const msg = JSON.parse(data.toString());
        received.push(msg.method);

        if (msg.method === "initialize") {
          ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: { authMethods: [{ id: "xai.api_key" }] } }));
        } else if (msg.method === "authenticate") {
          ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: {} }));
        } else if (msg.method === "session/new") {
          ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: { sessionId: "s1" } }));
        } else if (msg.method === "session/prompt") {
          ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: {} }));
        }
      });
    });

    originalStdin = process.stdin;
    originalExit = process.exit;
    const stdin = new PassThrough();
    setProcessStdin(stdin as any);
    setProcessExit((() => {}) as any);

    try {
      setTimeout(() => stdin.write("/quit\n"), 100);
      await connectCommand({ url: `${urlBase}/ws?server-key=test` });
    } finally {
      wss.clients.forEach((client) => client.close());
      await new Promise<void>((resolve) => wss.close(() => resolve()));
    }

    assert.ok(received.includes("initialize"));
    assert.ok(received.includes("session/new"));
  });
});
