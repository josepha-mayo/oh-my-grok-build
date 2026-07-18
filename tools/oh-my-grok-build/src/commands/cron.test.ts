import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { cronCommand } from "./cron.js";
import { listJobs, stopJob } from "../background/scheduler.js";
import { setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;

describe("cron command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
  });

  afterEach(() => {
    cleanupOmgHome(tempDir);
  });

  it("creates a scheduled cron job", async () => {
    await cronCommand({ expression: "0 0 1 1 *", prompt: "hello", model: "grok-2" });

    const jobs = await listJobs();
    assert.strictEqual(jobs.length, 1);
    assert.strictEqual(jobs[0].name, "cron");
    assert.strictEqual((jobs[0].meta as { model?: string } | undefined)?.model, "grok-2");
    assert.strictEqual((jobs[0].meta as { prompt?: string } | undefined)?.prompt, "hello");

    await stopJob("cron");
  });
});
