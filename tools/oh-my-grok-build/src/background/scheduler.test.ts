import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  startJob,
  stopJob,
  deleteJob,
  listJobs,
  runJobNow,
  isDaemonRunning,
  startSchedulerDaemon,
  stopSchedulerDaemon,
} from "./scheduler.js";

let tempDir: string;

describe("scheduler", () => {
  beforeEach(() => {
    tempDir = mkdtempSync(join(tmpdir(), "omgb-test-"));
    process.env.OMGB_HOME = tempDir;
  });

  afterEach(() => {
    delete process.env.OMGB_HOME;
    rmSync(tempDir, { recursive: true, force: true });
  });

  it("starts, lists, and stops a job", async () => {
    await startJob("test", "0 0 1 1 *", async () => {}, { prompt: "test prompt" });

    const jobs = await listJobs();
    assert.strictEqual(jobs.length, 1);
    assert.strictEqual(jobs[0].name, "test");
    assert.strictEqual(jobs[0].status, "active");
    assert.strictEqual(jobs[0].meta?.prompt, "test prompt");

    await stopJob("test");
    const stopped = await listJobs();
    assert.strictEqual(stopped[0].status, "stopped");
  });

  it("deletes a job", async () => {
    await startJob("x", "0 0 1 1 *", async () => {});
    await deleteJob("x");
    const jobs = await listJobs();
    assert.strictEqual(jobs.length, 0);
  });

  it("runs a job on demand", async () => {
    let ran = false;
    await startJob("now", "0 0 1 1 *", async () => {
      ran = true;
    });

    const ok = await runJobNow("now");
    assert.strictEqual(ok, true);
    assert.strictEqual(ran, true);

    await stopJob("now");
  });

  it("returns false for unknown jobs", async () => {
    const ok = await runJobNow("does-not-exist");
    assert.strictEqual(ok, false);
  });
});

describe("daemon lifecycle", () => {
  beforeEach(() => {
    tempDir = mkdtempSync(join(tmpdir(), "omgb-daemon-"));
    process.env.OMGB_HOME = tempDir;
  });

  afterEach(async () => {
    await stopSchedulerDaemon().catch(() => {});
    delete process.env.OMGB_HOME;
    rmSync(tempDir, { recursive: true, force: true });
  });

  it("starts, detects, and stops a daemon", async () => {
    assert.strictEqual(await isDaemonRunning(), false);
    const started = await startSchedulerDaemon();
    assert.strictEqual(started, true, "daemon should start");
    assert.strictEqual(await isDaemonRunning(), true, "daemon should be running after start");
    const stopped = await stopSchedulerDaemon();
    assert.strictEqual(stopped, true, "daemon should stop");
    assert.strictEqual(await isDaemonRunning(), false, "daemon should not be running after stop");
  });
});
