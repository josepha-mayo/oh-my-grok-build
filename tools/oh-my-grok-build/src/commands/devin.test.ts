import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { readFileSync } from "node:fs";
import { devinLoopCommand, devinAutonomousCommand } from "./devin.js";
import { saveGrokConfig } from "../config.js";
import spawner from "../spawner.js";
import { fakeProcess, setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;
let originalSpawn: unknown;
let originalSpawnSync: unknown;

describe("devin command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
    originalSpawn = (spawner as any).spawn;
    originalSpawnSync = (spawner as any).spawnSync;
  });

  afterEach(() => {
    (spawner as any).spawn = originalSpawn;
    (spawner as any).spawnSync = originalSpawnSync;
    cleanupOmgHome(tempDir);
  });

  it("errors when the working tree is dirty at the start", async () => {
    (spawner as any).spawnSync = () => ({ status: 0, stdout: " M file.ts\n" });
    await assert.rejects(devinLoopCommand({ prompt: "hello" }), /Working tree is not clean/);
  });

  it("iterates until the working tree is clean and logs iterations", async () => {
    const statuses = ["", " M file.ts\n", ""];
    const diffs = ["", "diff --git a/file.ts b/file.ts\n"];
    (spawner as any).spawnSync = (cmd: string, args: string[]) => {
      if (args[0] === "status") return { status: 0, stdout: statuses.shift() ?? "" };
      if (args[0] === "diff") return { status: 0, stdout: diffs.shift() ?? "" };
      return { status: 0, stdout: "" };
    };

    (spawner as any).spawn = () => {
      const proc = fakeProcess();
      proc.finish(0);
      return proc;
    };

    await devinLoopCommand({ prompt: "hello", maxIterations: 5 });

    const log = readFileSync(`${tempDir}/logs/devin-loop.jsonl`, "utf8").trim();
    const lines = log.split("\n").filter(Boolean);
    assert.strictEqual(lines.length, 2);
  });

  it("runs autonomous with yolo and warns about sandbox", async () => {
    await saveGrokConfig({ sandbox: { profile: "off" } });

    let captured: { args: string[]; env: NodeJS.ProcessEnv } | undefined;
    const warnings: string[] = [];
    const originalWarn = console.warn;
    console.warn = (...args: unknown[]) => warnings.push(args.join(" "));

    (spawner as any).spawn = (cmd: string, args: string[], options: { env: NodeJS.ProcessEnv }) => {
      captured = { args, env: options.env };
      const proc = fakeProcess();
      proc.finish(0);
      return proc;
    };

    try {
      await devinAutonomousCommand({ prompt: "hello" });
    } finally {
      console.warn = originalWarn;
    }

    assert.ok(captured);
    assert.ok(captured!.args.includes("--yolo"));
    assert.ok(warnings.some((w) => w.includes("sandbox")));
  });

  it("sets GROK_SANDBOX_PROFILE when --sandbox-profile is provided", async () => {
    let captured: { env: NodeJS.ProcessEnv } | undefined;
    (spawner as any).spawn = (cmd: string, args: string[], options: { env: NodeJS.ProcessEnv }) => {
      captured = { env: options.env };
      const proc = fakeProcess();
      proc.finish(0);
      return proc;
    };

    await devinAutonomousCommand({ prompt: "hello", sandboxProfile: "strict" });

    assert.ok(captured);
    assert.strictEqual(captured!.env.GROK_SANDBOX_PROFILE, "strict");
  });
});
