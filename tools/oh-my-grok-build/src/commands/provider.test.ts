import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";

import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import {
  providerAddCommand,
  providerListCommand,
  providerRemoveCommand,
  providerDefaultCommand,
  providerDiscoverCommand,
  providerTestCommand,
} from "./provider.js";
import { addProvider, getProvider, setDefaultProvider } from "../providers/manager.js";
import { setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;

function startMockServer(
  handler: (
    req: { url?: string },
    res: { writeHead: (code: number, headers?: Record<string, string>) => void; end: (data?: string) => void }
  ) => void
): Promise<{ baseUrl: string; close: () => Promise<void> }> {
  return new Promise((resolve, reject) => {
    const server = createServer((req, res) => handler(req, res));
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address() as AddressInfo;
      resolve({
        baseUrl: `http://127.0.0.1:${port}/v1`,
        close: () => new Promise<void>((r) => server.close(() => r())),
      });
    });
    server.on("error", reject);
  });
}

describe("provider command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
  });

  afterEach(() => {
    cleanupOmgHome(tempDir);
  });

  it("lists configured providers", async () => {
    await addProvider({ id: "x", name: "X", model: "m", baseUrl: "https://x" });

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await providerListCommand();
    } finally {
      console.log = original;
    }

    assert.ok(lines.some((l) => l.includes("omgb-x")));
  });

  it("adds a provider non-interactively from a preset", async () => {
    await providerAddCommand({ interactive: false, presetId: "openai" });

    const p = await getProvider("openai");
    assert.ok(p);
    assert.strictEqual(p!.model, "gpt-4o");
  });

  it("adds a custom provider non-interactively", async () => {
    await providerAddCommand({
      interactive: false,
      id: "my-local",
      baseUrl: "http://localhost:1234/v1",
      model: "mymodel",
    });

    const p = await getProvider("my-local");
    assert.ok(p);
    assert.strictEqual(p!.model, "mymodel");
    assert.strictEqual(p!.baseUrl, "http://localhost:1234/v1");
  });

  it("removes a provider", async () => {
    await addProvider({ id: "x", model: "m", baseUrl: "https://x" });
    await providerRemoveCommand("x");
    assert.strictEqual(await getProvider("x"), undefined);
  });

  it("sets the default provider", async () => {
    await addProvider({ id: "a", model: "m", baseUrl: "https://x" });
    await setDefaultProvider("a");
    const p = await providerDefaultCommand("a");
    assert.ok(p === undefined);
  });

  it("discovers no local models when none are reachable", async () => {
    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await providerDiscoverCommand();
    } finally {
      console.log = original;
    }
    assert.ok(lines.some((l) => l.includes("No local models discovered")));
  });

  it("tests a provider against a mock server", async () => {
    const server = await startMockServer((req, res) => {
      if (req.url === "/v1/models") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ data: [{ id: "m" }] }));
      } else {
        res.writeHead(404);
        res.end("not found");
      }
    });

    try {
      await addProvider({ id: "mock", name: "Mock", model: "m", baseUrl: server.baseUrl });

      const lines: string[] = [];
      const original = console.log;
      console.log = (...args: unknown[]) => lines.push(args.join(" "));
      try {
        await providerTestCommand("mock");
      } finally {
        console.log = original;
      }

      assert.ok(lines.some((l) => l.includes("reachable")));
    } finally {
      await server.close();
    }
  });
});
