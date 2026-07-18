import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { readFileSync } from "node:fs";
import { loopCommand } from "./loop.js";
import spawner from "../spawner.js";
import { fakeProcess, setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;
let originalSpawn: unknown;

describe("loop command", () => {
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
    await assert.rejects(loopCommand({ prompt: "hello" }), /Working tree is not clean/);
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

    await loopCommand({ prompt: "hello", maxIterations: 5 });

    const log = readFileSync(`${tempDir}/logs/loop.jsonl`, "utf8").trim();
    const lines = log.split("\n").filter(Boolean);
    assert.strictEqual(lines.length, 2);
  });
});
