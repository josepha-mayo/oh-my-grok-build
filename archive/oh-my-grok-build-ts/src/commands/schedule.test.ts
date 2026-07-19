import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { startJob, stopJob } from "../background/scheduler.js";
import { scheduleListCommand, scheduleStopCommand, scheduleRunCommand, scheduleDeleteCommand } from "./schedule.js";
import spawner from "../spawner.js";
import { fakeProcess, setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;
let originalSpawn: unknown;

describe("schedule command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
    originalSpawn = (spawner as any).spawn;
  });

  afterEach(() => {
    (spawner as any).spawn = originalSpawn;
    cleanupOmgHome(tempDir);
  });

  it("lists scheduled jobs", async () => {
    await startJob("daily", "0 0 * * *", async () => {}, { prompt: "hello" });

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await scheduleListCommand();
    } finally {
      console.log = original;
    }

    assert.ok(lines.some((l) => l.includes("daily")));
    await stopJob("daily");
  });

  it("stops a scheduled job", async () => {
    await startJob("daily", "0 0 * * *", async () => {}, {});

    const lines: string[] = [];
    const originalLog = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await scheduleStopCommand("daily");
    } finally {
      console.log = originalLog;
    }

    assert.ok(lines.some((l) => l.includes("Stopped job")));
  });

  it("runs a stored job through the runner", async () => {
    await startJob("daily", "0 0 * * *", async () => {}, { prompt: "hello", model: "grok-2" });
    await stopJob("daily");

    let captured: string[] = [];
    (spawner as any).spawn = (cmd: string, args: string[]) => {
      captured = args;
      const proc = fakeProcess();
      proc.finish(0);
      return proc;
    };

    const lines: string[] = [];
    const originalLog = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await scheduleRunCommand("daily");
    } finally {
      console.log = originalLog;
    }

    assert.ok(captured.includes("hello"));
    assert.ok(captured.includes("grok-2"));
    assert.ok(lines.some((l) => l.includes("Ran job from stored task")));
  });

  it("deletes a scheduled job", async () => {
    await startJob("daily", "0 0 * * *", async () => {}, {});
    await scheduleDeleteCommand("daily");

    const lines: string[] = [];
    const originalLog = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await scheduleListCommand();
    } finally {
      console.log = originalLog;
    }

    assert.ok(!lines.some((l) => l.includes("daily")));
  });
});
