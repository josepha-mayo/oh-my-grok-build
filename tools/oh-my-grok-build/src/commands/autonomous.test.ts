import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { devinAutonomousCommand } from "./devin.js";
import { saveGrokConfig } from "../config.js";
import spawner from "../spawner.js";
import { fakeProcess, setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;
let originalSpawn: unknown;

describe("autonomous command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
    originalSpawn = (spawner as any).spawn;
  });

  afterEach(() => {
    (spawner as any).spawn = originalSpawn;
    cleanupOmgHome(tempDir);
  });

  function grokProcess(code = 0) {
    const proc = fakeProcess();
    setImmediate(() => proc.finish(code));
    return proc;
  }

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
