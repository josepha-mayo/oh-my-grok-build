import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { Readable } from "node:stream";
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
let originalStdin: typeof process.stdin;

function setProcessStdin(value: typeof process.stdin) {
  Object.defineProperty(process, "stdin", { value, configurable: true });
}

describe("provider command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
  });

  afterEach(() => {
    if (originalStdin) setProcessStdin(originalStdin);
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

  it("adds a provider non-interactively", async () => {
    originalStdin = process.stdin;
    setProcessStdin(Readable.from(["sk-test\n"]) as any);

    await providerAddCommand(false, "openai");

    const p = await getProvider("openai");
    assert.ok(p);
    assert.strictEqual(p!.model, "gpt-4o");
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
    const server = createServer((req, res) => {
      if (req.url === "/v1/models") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ data: [{ id: "m" }] }));
      } else {
        res.writeHead(404);
        res.end("not found");
      }
    });

    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", () => resolve()));
    const { port } = server.address() as AddressInfo;

    try {
      await addProvider({ id: "mock", name: "Mock", model: "m", baseUrl: `http://127.0.0.1:${port}/v1` });

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
      await new Promise<void>((resolve) => server.close(() => resolve()));
    }
  });
});
