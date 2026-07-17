import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { teamCommand } from "./team.js";
import spawner from "../spawner.js";
import { fakeProcess, setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;
let originalSpawn: unknown;

describe("team command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
    originalSpawn = (spawner as any).spawn;
  });

  afterEach(() => {
    (spawner as any).spawn = originalSpawn;
    cleanupOmgHome(tempDir);
  });

  it("spawns the requested number of workers", async () => {
    const calls: string[][] = [];
    (spawner as any).spawn = (cmd: string, args: string[]) => {
      calls.push(args);
      const proc = fakeProcess(calls.length);
      setImmediate(() => proc.stdout.write(Buffer.from(`worker ${calls.length}`)));
      proc.finish(0);
      return proc;
    };

    await teamCommand({ count: 3, prompt: "do work" });

    assert.strictEqual(calls.length, 3);
    for (const args of calls) {
      assert.strictEqual(args[1], "do work");
    }
  });

  it("passes yolo and model to each worker", async () => {
    const calls: string[][] = [];
    (spawner as any).spawn = (cmd: string, args: string[]) => {
      calls.push(args);
      const proc = fakeProcess(calls.length);
      proc.finish(0);
      return proc;
    };

    await teamCommand({ count: 1, prompt: "do work", yolo: true, model: "grok-2" });

    assert.strictEqual(calls.length, 1);
    assert.ok(calls[0].includes("--yolo"));
    assert.ok(calls[0].includes("grok-2"));
  });
});
