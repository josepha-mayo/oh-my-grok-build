import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { loopCommand } from "./loop.js";
import { listJobs, stopJob } from "../background/scheduler.js";
import { setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;

describe("loop command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
  });

  afterEach(() => {
    cleanupOmgHome(tempDir);
  });

  it("creates a scheduled loop job", async () => {
    await loopCommand({ expression: "0 0 1 1 *", prompt: "hello", model: "grok-2" });

    const jobs = await listJobs();
    assert.strictEqual(jobs.length, 1);
    assert.strictEqual(jobs[0].name, "loop");
    assert.strictEqual((jobs[0].meta as { model?: string } | undefined)?.model, "grok-2");
    assert.strictEqual((jobs[0].meta as { prompt?: string } | undefined)?.prompt, "hello");

    await stopJob("loop");
  });
});
