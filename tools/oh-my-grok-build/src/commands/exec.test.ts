import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { execCommand } from "./exec.js";
import spawner from "../spawner.js";
import { fakeProcess, setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;
let originalSpawn: unknown;

describe("exec command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
    originalSpawn = (spawner as any).spawn;
  });

  afterEach(() => {
    (spawner as any).spawn = originalSpawn;
    cleanupOmgHome(tempDir);
  });

  it("spawns grok with the prompt and model", async () => {
    let captured: { cmd: string; args: string[] } | undefined;
    (spawner as any).spawn = (cmd: string, args: string[]) => {
      captured = { cmd, args };
      const proc = fakeProcess();
      proc.finish(0);
      return proc;
    };

    await execCommand({ prompt: "hello", model: "grok-2" });

    assert.ok(captured);
    assert.strictEqual(captured!.cmd, "grok");
    assert.strictEqual(captured!.args[1], "hello");
    assert.ok(captured!.args.includes("--model"));
    assert.ok(captured!.args.includes("grok-2"));
  });

  it("adds --yolo and --max-turns when requested", async () => {
    let captured: string[] = [];
    (spawner as any).spawn = (cmd: string, args: string[]) => {
      captured = args;
      const proc = fakeProcess();
      proc.finish(0);
      return proc;
    };

    await execCommand({ prompt: "hello", yolo: true, maxTurns: 3 });

    assert.ok(captured.includes("--yolo"));
    assert.ok(captured.includes("--max-turns"));
    assert.ok(captured.includes("3"));
  });

  it("rejects when grok exits non-zero", async () => {
    (spawner as any).spawn = () => {
      const proc = fakeProcess();
      proc.finish(1);
      return proc;
    };

    await assert.rejects(execCommand({ prompt: "fail" }), /exited with code 1/);
  });
});
