import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { PassThrough } from "node:stream";
import { WebSocketServer } from "ws";
import type { WebSocket } from "ws";
import { connectCommand } from "./connect.js";
import { setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;

function waitForPrompt(output: PassThrough, n = 1): Promise<void> {
  return new Promise((resolve) => {
    let count = 0;
    let buffer = "";
    const listener = (chunk: Buffer) => {
      buffer += chunk.toString();
      while (buffer.includes("you>")) {
        count++;
        buffer = buffer.slice(buffer.indexOf("you>") + 4);
        if (count >= n) {
          output.off("data", listener);
          resolve();
          return;
        }
      }
    };
    output.on("data", listener);
  });
}

describe("connect command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
  });

  afterEach(() => {
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

    const input = new PassThrough();
    const output = new PassThrough();
    const exit = (code: number) => output.write(`exit:${code}\n`);

    try {
      waitForPrompt(output).then(() => input.write("/quit\n"));
      await connectCommand({
        url: `${urlBase}/ws?server-key=test`,
        input,
        output,
        exit,
      });
    } finally {
      wss.clients.forEach((client) => client.close());
      await new Promise<void>((resolve) => wss.close(() => resolve()));
    }

    assert.ok(received.includes("initialize"));
    assert.ok(received.includes("session/new"));
  });

  it("waits for turn_completed before each /loop iteration", async () => {
    const wss = new WebSocketServer({ host: "127.0.0.1", port: 0 });
    const urlBase = await new Promise<string>((resolve) => {
      wss.on("listening", () => {
        const addr = wss.address();
        resolve(`ws://127.0.0.1:${(addr as { port: number }).port}`);
      });
    });

    const received: string[] = [];
    let promptCount = 0;
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
          promptCount++;
          ws.send(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result: {} }));
          setTimeout(() => {
            ws.send(
              JSON.stringify({
                jsonrpc: "2.0",
                method: "session/update",
                params: {
                  sessionId: "s1",
                  update: { sessionUpdate: "turn_completed" },
                },
              })
            );
          }, 20);
        }
      });
    });

    const input = new PassThrough();
    const output = new PassThrough();
    const exit = (code: number) => output.write(`exit:${code}\n`);

    try {
      waitForPrompt(output).then(() => input.write("/loop 2 hello\n"));
      waitForPrompt(output, 2).then(() => input.write("/quit\n"));
      await connectCommand({
        url: `${urlBase}/ws?server-key=test`,
        input,
        output,
        exit,
      });
    } finally {
      wss.clients.forEach((client) => client.close());
      await new Promise<void>((resolve) => wss.close(() => resolve()));
    }

    assert.strictEqual(promptCount, 2);
    assert.ok(received.filter((m) => m === "session/prompt").length >= 2);
  });
});
