import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { probeOllama, probeLmStudio, discoverLocalModels, resolveApiKey, testProvider } from "./local.js";
import type { ProviderConfig } from "../types.js";

let tempDir: string;

function makeTempDir(): string {
  return mkdtempSync(join(tmpdir(), "omgb-local-test-"));
}

beforeEach(() => {
  tempDir = makeTempDir();
  process.env.OMGB_HOME = tempDir;
  process.env.GROK_HOME = join(tempDir, ".grok");
});

afterEach(() => {
  delete process.env.OMGB_HOME;
  delete process.env.GROK_HOME;
  delete process.env.OMGB_LOCALTEST_ENV;
  rmSync(tempDir, { recursive: true, force: true });
});

function startMockServer(
  handler: (req: { url?: string }, res: { writeHead: (code: number, headers?: Record<string, string>) => void; end: (data?: string) => void }) => void
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

describe("resolveApiKey", () => {
  it("resolves from an environment variable", async () => {
    process.env.OMGB_LOCALTEST_ENV = "sk-env";
    const provider: ProviderConfig = {
      id: "test",
      name: "Test",
      model: "m",
      baseUrl: "http://localhost/v1",
      envKey: ["OMGB_LOCALTEST_ENV"],
    };
    const key = await resolveApiKey(provider);
    assert.strictEqual(key, "sk-env");
  });

  it("resolves from ~/.omgb/.env when env var is not set", async () => {
    writeFileSync(join(tempDir, ".env"), "OMGB_LOCALTEST_ENV=sk-dotenv\n");
    const provider: ProviderConfig = {
      id: "test",
      name: "Test",
      model: "m",
      baseUrl: "http://localhost/v1",
      envKey: ["OMGB_LOCALTEST_ENV"],
    };
    const key = await resolveApiKey(provider);
    assert.strictEqual(key, "sk-dotenv");
  });

  it("prefers the inline apiKey when present", async () => {
    process.env.OMGB_LOCALTEST_ENV = "sk-env";
    const provider: ProviderConfig = {
      id: "test",
      name: "Test",
      model: "m",
      baseUrl: "http://localhost/v1",
      apiKey: "sk-direct",
      envKey: ["OMGB_LOCALTEST_ENV"],
    };
    const key = await resolveApiKey(provider);
    assert.strictEqual(key, "sk-direct");
  });
});

describe("probeOllama", () => {
  it("lists models from an OpenAI-compatible /models endpoint", async () => {
    const server = await startMockServer((req, res) => {
      if (req.url === "/v1/models") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ data: [{ id: "llama3" }, { id: "codellama" }] }));
      } else {
        res.writeHead(404);
        res.end("not found");
      }
    });
    try {
      const models = await probeOllama(server.baseUrl);
      assert.deepStrictEqual(models, ["llama3", "codellama"]);
    } finally {
      await server.close();
    }
  });

  it("returns an empty array for unreachable hosts", async () => {
    const models = await probeOllama("http://127.0.0.1:1/v1");
    assert.deepStrictEqual(models, []);
  });
});

describe("probeLmStudio", () => {
  it("lists models from an OpenAI-compatible /models endpoint", async () => {
    const server = await startMockServer((req, res) => {
      if (req.url === "/v1/models") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ data: [{ id: "local-model" }] }));
      } else {
        res.writeHead(404);
        res.end("not found");
      }
    });
    try {
      const models = await probeLmStudio(server.baseUrl);
      assert.deepStrictEqual(models, ["local-model"]);
    } finally {
      await server.close();
    }
  });

  it("returns an empty array for unreachable hosts", async () => {
    const models = await probeLmStudio("http://127.0.0.1:1/v1");
    assert.deepStrictEqual(models, []);
  });
});

describe("discoverLocalModels", () => {
  it("returns nothing when no local servers are reachable", async () => {
    const found = await discoverLocalModels({
      ollama: "http://127.0.0.1:1/v1",
      lmstudio: "http://127.0.0.1:1/v1",
    });
    assert.deepStrictEqual(found, []);
  });

  it("returns discovered providers with models", async () => {
    const server = await startMockServer((req, res) => {
      if (req.url === "/v1/models") {
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ data: [{ id: "model-a" }] }));
      } else {
        res.writeHead(404);
        res.end("not found");
      }
    });
    try {
      const found = await discoverLocalModels({ ollama: server.baseUrl, lmstudio: server.baseUrl });
      assert.strictEqual(found.length, 2);
      assert.ok(found.some((g) => g.provider === "ollama" && g.models.includes("model-a")));
      assert.ok(found.some((g) => g.provider === "lmstudio" && g.models.includes("model-a")));
    } finally {
      await server.close();
    }
  });
});

describe("testProvider", () => {
  it("returns ok=true when /models responds", async () => {
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
      const provider: ProviderConfig = {
        id: "mock",
        name: "Mock",
        model: "m",
        baseUrl: server.baseUrl,
        apiBackend: "chat_completions",
      };
      const result = await testProvider(provider);
      assert.strictEqual(result.ok, true);
    } finally {
      await server.close();
    }
  });

  it("returns ok=false when the server errors", async () => {
    const server = await startMockServer((req, res) => {
      res.writeHead(500);
      res.end("boom");
    });
    try {
      const provider: ProviderConfig = {
        id: "mock",
        name: "Mock",
        model: "m",
        baseUrl: server.baseUrl,
        apiBackend: "chat_completions",
      };
      const result = await testProvider(provider);
      assert.strictEqual(result.ok, false);
      assert.ok(result.error);
    } finally {
      await server.close();
    }
  });
});
