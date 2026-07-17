import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { swarmCommand } from "./swarm.js";
import spawner from "../spawner.js";
import { fakeProcess, setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;
let originalSpawn: unknown;
let originalSpawnSync: unknown;

describe("swarm command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
    originalSpawn = (spawner as any).spawn;
    originalSpawnSync = (spawner as any).spawnSync;
    (spawner as any).spawnSync = () => ({ status: 1, stdout: "" });
  });

  afterEach(() => {
    (spawner as any).spawn = originalSpawn;
    (spawner as any).spawnSync = originalSpawnSync;
    cleanupOmgHome(tempDir);
  });

  it("decomposes a task and spawns subagents for each subtask", async () => {
    const calls: { args: string[] }[] = [];
    (spawner as any).spawn = (cmd: string, args: string[]) => {
      calls.push({ args });
      const proc = fakeProcess(calls.length);

      if (typeof args[1] === "string" && args[1].startsWith("Decompose")) {
        setImmediate(() => {
          proc.stdout.write(Buffer.from(JSON.stringify(["subtask a", "subtask b"])));
          proc.finish(0);
        });
      }

      return proc;
    };

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await swarmCommand({ prompt: "build a thing", workers: 2, timeout: 1000 });
    } finally {
      console.log = original;
    }

    assert.strictEqual(calls.length, 3); // decompose + 2 subagents
    assert.ok(calls.some((c) => c.args[1] === "subtask a"));
    assert.ok(calls.some((c) => c.args[1] === "subtask b"));
    assert.ok(lines.some((l) => l.includes("Aggregated results")));
  });

  it("limits workers to the configured maximum", async () => {
    const calls: { args: string[] }[] = [];
    (spawner as any).spawn = (cmd: string, args: string[]) => {
      calls.push({ args });
      const proc = fakeProcess(calls.length);

      if (typeof args[1] === "string" && args[1].startsWith("Decompose")) {
        setImmediate(() => {
          proc.stdout.write(Buffer.from(JSON.stringify(["a", "b", "c", "d", "e", "f"])));
          proc.finish(0);
        });
      }

      return proc;
    };

    await swarmCommand({ prompt: "build", workers: 3, timeout: 1000 });
    assert.strictEqual(calls.length, 4); // decompose + 3 capped subagents
  });
});
