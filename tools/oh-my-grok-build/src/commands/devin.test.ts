import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { readFileSync } from "node:fs";
import { devinLoopCommand, devinAutonomousCommand } from "./devin.js";
import { saveGrokConfig } from "../config.js";
import spawner from "../spawner.js";
import { fakeProcess, setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;
let originalSpawn: unknown;

describe("devin command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
    originalSpawn = (spawner as any).spawn;
  });

  afterEach(() => {
    (spawner as any).spawn = originalSpawn;
    cleanupOmgHome(tempDir);
  });

  function gitProcess(stdoutText: string) {
    const proc = fakeProcess();
    setImmediate(() => {
      proc.stdout.write(stdoutText);
      proc.finish(0);
    });
    return proc;
  }

  function grokProcess(code = 0) {
    const proc = fakeProcess();
    setImmediate(() => proc.finish(code));
    return proc;
  }

  it("errors when the working tree is dirty at the start", async () => {
    (spawner as any).spawn = (cmd: string, args: string[]) => {
      if (cmd === "git" && args[0] === "status") return gitProcess(" M file.ts\n");
      return grokProcess();
    };
    await assert.rejects(devinLoopCommand({ prompt: "hello" }), /Working tree is not clean/);
  });

  it("iterates until the working tree is clean and logs iterations", async () => {
    const statuses = ["", " M file.ts\n", ""];
    const diffs = ["", "diff --git a/file.ts b/file.ts\n"];

    (spawner as any).spawn = (cmd: string, args: string[]) => {
      if (cmd === "git") {
        if (args[0] === "status") return gitProcess(statuses.shift() ?? "");
        if (args[0] === "diff") return gitProcess(diffs.shift() ?? "");
      }
      if (cmd === "grok") return grokProcess(0);
      return grokProcess();
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
      if (cmd !== "grok") return grokProcess();
      captured = { args, env: options.env };
      return grokProcess(0);
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
      if (cmd !== "grok") return grokProcess();
      captured = { env: options.env };
      return grokProcess(0);
    };

    await devinAutonomousCommand({ prompt: "hello", sandboxProfile: "strict" });

    assert.ok(captured);
    assert.strictEqual(captured!.env.GROK_SANDBOX_PROFILE, "strict");
  });
});
