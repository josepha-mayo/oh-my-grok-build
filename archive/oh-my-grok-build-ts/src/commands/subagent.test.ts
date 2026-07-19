import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { writeFileSync } from "node:fs";
import { subagentSpawnCommand, subagentListCommand, subagentKillCommand, subagentLogsCommand } from "./subagent.js";
import { loadRegistry } from "../subagents/engine.js";
import spawner from "../spawner.js";
import { fakeProcess, setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;
let originalSpawn: unknown;
let originalSpawnSync: unknown;

describe("subagent command", () => {
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

  it("spawns and lists subagents", async () => {
    (spawner as any).spawn = () => fakeProcess(123);

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await subagentSpawnCommand({ name: "agent-1", prompt: "test prompt" });
      await subagentListCommand();
    } finally {
      console.log = original;
    }

    assert.ok(lines.some((l) => l.includes("Spawned subagent")));
    assert.ok(lines.some((l) => l.includes("agent-1")));
  });

  it("kills a subagent", async () => {
    (spawner as any).spawn = () => fakeProcess(123);
    await subagentSpawnCommand({ name: "agent-1", prompt: "test prompt" });

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await subagentKillCommand("agent-1");
      await subagentListCommand();
    } finally {
      console.log = original;
    }

    assert.ok(lines.some((l) => l.includes("Killed subagent")));
    assert.ok(!lines.some((l) => l.includes("agent-1") && !l.includes("Killed")));
  });

  it("shows subagent logs", async () => {
    (spawner as any).spawn = () => fakeProcess(123);
    await subagentSpawnCommand({ name: "agent-1", prompt: "test prompt" });

    const registry = await loadRegistry();
    const logPath = registry.find((r) => r.name === "agent-1")?.logPath;
    if (logPath) writeFileSync(logPath, "log output\n", { flag: "a" });

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await subagentLogsCommand("agent-1", 10);
    } finally {
      console.log = original;
    }

    assert.ok(lines.some((l) => l.includes("log output")));
  });
});
