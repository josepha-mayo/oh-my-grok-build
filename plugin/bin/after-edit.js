#!/usr/bin/env node
const { spawn } = require("child_process");
const fs = require("fs");

const input = [];
process.stdin.on("data", (d) => input.push(d));
process.stdin.on("end", () => {
  let payload = {};
  try {
    payload = JSON.parse(input.join(""));
  } catch {
    // Not a blocker; the edit itself succeeded.
    process.exit(0);
  }

  const filePath = payload.toolInput?.path ?? payload.toolInput?.filePath ?? payload.toolInput?.file ?? null;
  if (!filePath || !fs.existsSync(filePath)) {
    process.exit(0);
  }

  // Run git diff --check for the edited file to warn about whitespace errors.
  // The hook intentionally exits 0; its purpose is to surface warnings, not block edits.
  const proc = spawn("git", ["diff", "--check", "--", filePath], {
    stdio: ["ignore", "ignore", "pipe"],
  });

  proc.on("exit", (code) => {
    if (code !== 0 && code !== null) {
      console.error(`[after-edit] whitespace/style warning for ${filePath}`);
    }
    process.exit(0);
  });

  proc.on("error", () => {
    // git not available or not a repository; ignore.
    process.exit(0);
  });
});
