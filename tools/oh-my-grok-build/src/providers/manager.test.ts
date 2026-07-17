import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { addProvider, listProviders, getProvider, removeProvider, setDefaultProvider } from "./manager.js";
import { loadOmgConfig } from "../config.js";

let tempDir: string;

describe("provider manager", () => {
  beforeEach(() => {
    tempDir = mkdtempSync(join(tmpdir(), "omgb-test-"));
    process.env.OMGB_HOME = tempDir;
    process.env.GROK_HOME = join(tempDir, ".grok");
  });

  afterEach(() => {
    delete process.env.OMGB_HOME;
    delete process.env.GROK_HOME;
    rmSync(tempDir, { recursive: true, force: true });
  });

  it("adds a provider and persists it", async () => {
    const p = await addProvider({
      id: "openai-test",
      name: "OpenAI Test",
      model: "gpt-test",
      baseUrl: "https://api.test.example/v1",
      apiKey: "sk-test",
    });

    assert.strictEqual(p.id, "openai-test");
    assert.strictEqual(p.model, "gpt-test");
    assert.ok(Array.isArray(p.envKey));

    const providers = await listProviders();
    assert.strictEqual(providers.length, 1);
    assert.strictEqual(providers[0].name, "OpenAI Test");

    const cfg = await loadOmgConfig();
    assert.strictEqual(cfg.defaultModel, "omgb-openai-test");
  });

  it("removes a provider", async () => {
    await addProvider({ id: "a", model: "m", baseUrl: "https://x" });
    await removeProvider("a");
    const providers = await listProviders();
    assert.strictEqual(providers.length, 0);
  });

  it("sets default provider", async () => {
    await addProvider({ id: "a", model: "m", baseUrl: "https://x" });
    await addProvider({ id: "b", model: "m2", baseUrl: "https://y" });
    await setDefaultProvider("b");
    const cfg = await loadOmgConfig();
    assert.strictEqual(cfg.defaultModel, "omgb-b");
  });

  it("gets provider by id", async () => {
    await addProvider({ id: "x", model: "m", baseUrl: "https://x" });
    const p = await getProvider("x");
    assert.ok(p);
    assert.strictEqual(p!.id, "x");
  });
});
