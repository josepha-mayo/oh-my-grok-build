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
    "rm -rf ~/docs/project",
    "xargs echo",
    "env -S 'echo hello'",
    "env VAR=1 echo hello",
    'xargs echo "hello world"',
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
    ["cat ~/.ssh/id_rsa", "Blocked tilde expansion outside quotes"],
    ["xargs -0 rm -rf", "Blocked xargs with unanalyzed command: rm"],
    ["xargs -a ~/.ssh/id_rsa echo", "Blocked tilde expansion in xargs option value"],
    ["xargs -I PLACEHOLDER rm -rf PLACEHOLDER", "Blocked xargs argument replacement"],
    ["xargs --verbose rm", "Blocked xargs with unanalyzed command: rm"],
    ["xargs -S rm", "Blocked xargs with unanalyzed command: rm"],
    ["xargs --show-limits rm", "Blocked xargs with unanalyzed command: rm"],
    ["xargs -e rm", "Blocked xargs with unanalyzed command: rm"],
    ["xargs --eof rm", "Blocked xargs with unanalyzed command: rm"],
    ["xargs -- rm", "Blocked xargs with unanalyzed command: rm"],
    ["xargs -a /etc/passwd -- rm", "Blocked xargs with unanalyzed command: rm"],
    ["cmd /c del /f C:\\\\temp", "Blocked destructive Windows delete"],
    ["wsl -e node script.js", "Blocked unanalyzable command: node"],
    ["wsl -e bash -c 'rm -rf /'", "Blocked rm -rf on a dangerous target"],
    ["echo $(whoami)", "Blocked command substitution"],
    ["echo `whoami`", "Blocked command substitution (backtick)"],
    ["dd if=/dev/zero of=/dev/sda", "Blocked dd writing to a raw device"],
    ["format C:", "Blocked potentially destructive command: format"],
    ["shutdown now", "Blocked potentially destructive command: shutdown"],
    ["env -S 'rm -rf /'", "Blocked rm -rf on a dangerous target"],
    ["env --split-string='rm -rf /'", "Blocked rm -rf on a dangerous target"],
    ["env -S bash -c 'rm -rf /'", "Blocked rm -rf on a dangerous target"],
    ['env -S \'bash -c "rm -rf /"\'', "Blocked rm -rf on a dangerous target"],
    ['python3.11 -c \'import os; os.system("rm -rf /")\'', "Blocked unanalyzable command: python"],
    ["ksh93 -c 'rm -rf /'", "Blocked rm -rf on a dangerous target"],
    ['mawk \'BEGIN{system("rm -rf /")}\'', "Blocked unanalyzable command: mawk"],
    ["busybox sh -c 'rm -rf /'", "Blocked rm -rf on a dangerous target"],
    ["docker run --rm -v /:/host alpine rm -rf /host", "Blocked unanalyzable command: docker"],
    ["xargs printf '%n'", "Blocked xargs with unanalyzed command: printf"],
    ["unshare -r /bin/sh", "Blocked unanalyzable command: unshare"],
    ["pkexec rm -rf /", "Blocked unanalyzable command: pkexec"],
    // Windows executable/script extensions must not bypass the block lists.
    ["format.com C:", "Blocked potentially destructive command: format"],
    ["format.bat /y", "Blocked potentially destructive command: format"],
    ["diskpart.bat /s script.txt", "Blocked potentially destructive command: diskpart"],
    ["shutdown.bat /s /t 0", "Blocked potentially destructive command: shutdown"],
    ["cmd /c format.com C:", "Blocked potentially destructive command: format"],
    ["python3.11.bat -c 'import os; os.system(\"rm -rf /\")'", "Blocked unanalyzable command: python"],
    ["node.js script.js", "Blocked unanalyzable command: node"],
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
