import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { modelCommand, modelsCommand } from "./model.js";
import { loadGrokConfig, saveGrokConfig, loadOmgConfig } from "../config.js";
import { setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;

describe("model command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
  });

  afterEach(() => {
    cleanupOmgHome(tempDir);
  });

  it("sets the default model in both configs", async () => {
    await modelCommand("omgb-test");

    const ocfg = await loadOmgConfig();
    assert.strictEqual(ocfg.defaultModel, "omgb-test");

    const gcfg = await loadGrokConfig();
    assert.strictEqual((gcfg.models as Record<string, unknown> | undefined)?.default, "omgb-test");
  });

  it("reads the current default model", async () => {
    await modelCommand("omgb-test");

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await modelCommand();
    } finally {
      console.log = original;
    }

    assert.ok(lines.some((l) => l.includes("omgb-test")));
  });

  it("lists custom models", async () => {
    const gcfg = await loadGrokConfig();
    gcfg.model = { "omgb-test": { name: "Test", model: "gpt-test" } };
    await saveGrokConfig(gcfg);

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await modelsCommand();
    } finally {
      console.log = original;
    }

    assert.ok(lines.some((l) => l.includes("omgb-test")));
  });
});
