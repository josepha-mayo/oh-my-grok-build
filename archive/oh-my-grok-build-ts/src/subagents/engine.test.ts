import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { saveRegistry, listSubagents, isRunning } from "./engine.js";

let tempDir: string;

describe("subagents engine", () => {
  beforeEach(() => {
    tempDir = mkdtempSync(join(tmpdir(), "omgb-test-"));
    process.env.OMGB_HOME = tempDir;
  });

  afterEach(() => {
    delete process.env.OMGB_HOME;
    rmSync(tempDir, { recursive: true, force: true });
  });

  it("persists and lists subagents without spawning", async () => {
    const record = {
      name: "test-agent",
      pid: 99999,
      worktree: "/tmp/test-agent",
      logPath: "/tmp/test-agent/subagent.log",
      prompt: "test prompt",
      spawnedAt: new Date().toISOString(),
    };

    await saveRegistry([record]);
    const agents = await listSubagents();

    assert.strictEqual(agents.length, 1);
    assert.strictEqual(agents[0].name, "test-agent");
    assert.strictEqual(agents[0].running, false);
  });

  it("reports a fake pid as not running", () => {
    assert.strictEqual(isRunning(99999), false);
  });
});
