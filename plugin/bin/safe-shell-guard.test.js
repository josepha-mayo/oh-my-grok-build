const { describe, it } = require("node:test");
const assert = require("node:assert");
const { spawn } = require("node:child_process");
const { join } = require("node:path");

const GUARD = join(__dirname, "safe-shell-guard.js");

function runGuard(command) {
  return new Promise((resolve, reject) => {
    const proc = spawn(process.execPath, [GUARD], { cwd: __dirname });
    const out = [];
    const err = [];
    proc.stdout.on("data", (d) => out.push(d));
    proc.stderr.on("data", (d) => err.push(d));
    proc.on("error", reject);
    proc.on("close", (code) => {
      try {
        const result = JSON.parse(Buffer.concat(out).toString("utf8"));
        resolve({ ...result, code, stderr: Buffer.concat(err).toString("utf8") });
      } catch {
        resolve({
          decision: "unknown",
          reason: Buffer.concat(out).toString("utf8") || Buffer.concat(err).toString("utf8"),
          code,
        });
      }
    });
    proc.stdin.end(JSON.stringify({ toolInput: { command } }));
  });
}

describe("safe-shell-guard", () => {
  const safe = [
    "git status --short",
    "cat README.md",
    "grep -r foo src",
    "npm run build",
    'echo "hello world"',
    "bash -c 'git status'",
    "bash -c 'echo hello'",
    "rm -rf node_modules/.cache",
    "rm -rf /home/user/docs/project",
    "rm -rf /tmp/*.log",
  ];

  for (const cmd of safe) {
    it(`allows: ${cmd}`, async () => {
      const result = await runGuard(cmd);
      assert.strictEqual(result.decision, "allow", `expected allow, got ${result.reason}`);
      assert.strictEqual(result.code, 0);
    });
  }

  const dangerous = [
    ["rm -rf /", "Blocked rm -rf on a dangerous target"],
    ["rm -rf ~", "Blocked rm -rf on a dangerous target"],
    ["rm -rf ~/*", "Blocked rm -rf on a dangerous target"],
    ["rm -rf /.*", "Blocked rm -rf on a dangerous target"],
    ["rm -rf .*", "Blocked rm -rf on a dangerous target"],
    ["rm -rf .", "Blocked rm -rf on a dangerous target"],
    ["rm -rf ..", "Blocked rm -rf on a dangerous target"],
    ["rm -rf ../../", "Blocked rm -rf on a dangerous target"],
    ["sudo mkfs", "Blocked potentially destructive command: mkfs"],
    ["sudo -u root rm -rf /", "Blocked rm -rf on a dangerous target"],
    ["nice -n 10 rm -rf /", "Blocked rm -rf on a dangerous target"],
    ["bash -c \"rm -rf /\"", "Blocked rm -rf on a dangerous target"],
    ["bash -c 'rm -rf /'", "Blocked rm -rf on a dangerous target"],
    ["cmd /c \"del /f C:\\\\temp\"", "Blocked destructive Windows delete"],
    ["cat \"$HOME/.ssh/id_rsa\"", "Blocked variable expansion inside double quotes"],
    ["echo $(whoami)", "Blocked command substitution"],
    ["echo `whoami`", "Blocked command substitution (backtick)"],
    ["dd if=/dev/zero of=/dev/sda", "Blocked dd writing to a raw device"],
    ["format C:", "Blocked potentially destructive command: format"],
    ["shutdown now", "Blocked potentially destructive command: shutdown"],
  ];

  for (const [cmd, expectedReason] of dangerous) {
    it(`blocks: ${cmd}`, async () => {
      const result = await runGuard(cmd);
      assert.strictEqual(result.decision, "deny", `expected deny for ${cmd}`);
      assert.strictEqual(result.code, 2);
      if (expectedReason) {
        assert.ok(
          result.reason === expectedReason || result.reason.includes(expectedReason),
          `expected reason containing "${expectedReason}", got "${result.reason}" for ${cmd}`
        );
      }
    });
  }

  it("blocks shell metacharacters", async () => {
    const metas = ["echo a; rm -rf /", "echo a && rm -rf /", "echo a | cat", "echo a > file"];
    for (const cmd of metas) {
      const result = await runGuard(cmd);
      assert.strictEqual(result.decision, "deny", `expected deny for ${cmd}`);
    }
  });

  it("rejects invalid payload", async () => {
    const proc = spawn(process.execPath, [GUARD], { cwd: __dirname });
    const out = [];
    proc.stdout.on("data", (d) => out.push(d));
    proc.stdin.end("not-json");
    const code = await new Promise((resolve) => proc.on("close", resolve));
    const result = JSON.parse(Buffer.concat(out).toString("utf8"));
    assert.strictEqual(result.decision, "deny");
    assert.strictEqual(code, 2);
  });
});
