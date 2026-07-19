import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { harnessAddCommand, harnessListCommand, harnessRemoveCommand, harnessRunCommand } from "./harness.js";
import spawner from "../spawner.js";
import { fakeProcess, setupOmgHome, cleanupOmgHome } from "../test-utils.js";

let tempDir: string;
let originalSpawn: unknown;

describe("harness command", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
    originalSpawn = (spawner as any).spawn;
  });

  afterEach(() => {
    (spawner as any).spawn = originalSpawn;
    cleanupOmgHome(tempDir);
  });

  it("adds and lists connectors", async () => {
    await harnessAddCommand("claude-main", "claude", { command: "claude" });

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await harnessListCommand();
    } finally {
      console.log = original;
    }

    assert.ok(lines.some((l) => l.includes("claude-main")));
  });

  it("removes a connector", async () => {
    await harnessAddCommand("x", "claude", {});
    await harnessRemoveCommand("x");

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await harnessListCommand();
    } finally {
      console.log = original;
    }
    assert.ok(!lines.some((l) => l.includes("x")));
  });

  it("runs a prompt through a connector", async () => {
    await harnessAddCommand("claude-main", "claude", { command: "claude" });

    let captured: { cmd: string; args: string[] } | undefined;
    (spawner as any).spawn = (cmd: string, args: string[]) => {
      captured = { cmd, args };
      const proc = fakeProcess();
      setImmediate(() => proc.stdout.write(Buffer.from("hello harness")));
      proc.finish(0);
      return proc;
    };

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await harnessRunCommand("claude-main", "do work");
    } finally {
      console.log = original;
    }

    assert.ok(captured);
    assert.strictEqual(captured!.cmd, "claude");
    assert.ok(captured!.args.includes("do work"));
    assert.ok(lines.some((l) => l.includes("hello harness")));
  });

  it("runs a prompt through the pi connector", async () => {
    await harnessAddCommand("pi-main", "pi", { command: "pi" });

    let captured: { cmd: string; args: string[] } | undefined;
    (spawner as any).spawn = (cmd: string, args: string[]) => {
      captured = { cmd, args };
      const proc = fakeProcess();
      setImmediate(() => proc.stdout.write(Buffer.from("pi output")));
      proc.finish(0);
      return proc;
    };

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await harnessRunCommand("pi-main", "do work");
    } finally {
      console.log = original;
    }

    assert.ok(captured);
    assert.strictEqual(captured!.cmd, "pi");
    assert.ok(captured!.args.includes("-p"));
    assert.ok(captured!.args.includes("do work"));
    assert.ok(lines.some((l) => l.includes("pi output")));
  });

  it("runs a prompt through the codex connector and logs in when needed", async () => {
    await harnessAddCommand("codex-main", "codex", { command: "codex", secret: "sk-test" });

    const calls: { cmd: string; args: string[] }[] = [];
    (spawner as any).spawn = (cmd: string, args: string[]) => {
      calls.push({ cmd, args });
      const proc = fakeProcess();
      if (cmd === "codex" && args[0] === "login" && args[1] === "status") {
        proc.finish(1);
      } else if (cmd === "codex" && args[0] === "login" && args[1] === "--with-api-key") {
        proc.finish(0);
      } else if (cmd === "codex" && args[0] === "exec") {
        setImmediate(() => proc.stdout.write(Buffer.from("codex output")));
        proc.finish(0);
      } else {
        proc.finish(0);
      }
      return proc;
    };

    const lines: string[] = [];
    const original = console.log;
    console.log = (...args: unknown[]) => lines.push(args.join(" "));
    try {
      await harnessRunCommand("codex-main", "do work");
    } finally {
      console.log = original;
    }

    assert.ok(calls.some((c) => c.cmd === "codex" && c.args[0] === "login" && c.args[1] === "--with-api-key"));
    assert.ok(calls.some((c) => c.cmd === "codex" && c.args[0] === "exec"));
    assert.ok(lines.some((l) => l.includes("codex output")));
  });
});
