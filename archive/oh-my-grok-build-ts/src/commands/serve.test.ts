import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { WebSocket } from "ws";
import { serveCommand } from "./serve.js";
import { stopAgentServer } from "../acp/server.js";
import spawner from "../spawner.js";
import { fakeProcess, setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;
let originalSpawn: unknown;

describe("serve command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
    originalSpawn = (spawner as any).spawn;
  });

  afterEach(() => {
    (spawner as any).spawn = originalSpawn;
    cleanupOmgHome(tempDir);
  });

  it("starts an agent server and returns its URL", async () => {
    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));

    const server = await serveCommand({ bind: "127.0.0.1", port: 0, qr: false });
    try {
      console.log = original;
      assert.ok(server.url.startsWith("ws://"));
      assert.ok(lines.some((l) => l.includes("agent server is running")));
    } finally {
      await stopAgentServer(server);
    }
  });

  it("rejects connections with an invalid server key", async () => {
    const server = await serveCommand({ bind: "127.0.0.1", port: 0, qr: false });
    try {
      const url = server.url.replace(/server-key=[^&]+/, "server-key=bad");
      const client = new WebSocket(url);
      const code = await new Promise<number>((resolve) => {
        client.on("close", (code) => resolve(code));
        client.on("error", () => resolve(-1));
      });
      assert.strictEqual(code, 1008);
    } finally {
      await stopAgentServer(server);
    }
  });

  it("spawns grok when a valid client connects", async () => {
    let captured: { cmd: string; args: string[] } | undefined;
    (spawner as any).spawn = (cmd: string, args: string[]) => {
      captured = { cmd, args };
      return fakeProcess(1);
    };

    const server = await serveCommand({ bind: "127.0.0.1", port: 0, qr: false });
    const client = new WebSocket(server.url);
    try {
      await new Promise<void>((resolve, reject) => {
        client.on("open", () => resolve());
        client.on("error", reject);
      });

      assert.ok(captured);
      assert.strictEqual(captured!.cmd, "grok");
      assert.deepStrictEqual(captured!.args, ["agent", "--no-leader", "stdio"]);
    } finally {
      client.close();
      await stopAgentServer(server);
    }
  });
});
