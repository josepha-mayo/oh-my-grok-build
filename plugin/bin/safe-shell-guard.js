#!/usr/bin/env node
const input = [];
process.stdin.on("data", (d) => input.push(d));
process.stdin.on("end", () => {
  const payload = JSON.parse(input.join(""));
  const cmd = payload.toolInput?.command ?? "";

  if (isDangerous(cmd)) {
    console.log(
      JSON.stringify({
        decision: "deny",
        reason: "Blocked potentially destructive command by oh-my-grok-build safe-shell guard",
      })
    );
    process.exit(2);
  }

  console.log(JSON.stringify({ decision: "allow" }));
});

function isDangerous(command) {
  // Split on common shell separators so each logical segment is checked independently.
  const segments = command.split(/;|&&|\|\||\||`|\$\([^)]*\)|\r?\n/);

  for (const segment of segments) {
    const trimmed = segment.trim();
    if (!trimmed) continue;

    if (isDangerousSegment(trimmed)) return true;
  }

  return false;
}

function isDangerousSegment(segment) {
  const lower = segment.toLowerCase();

  // Fork bomb
  if (/:\(\)\s*\{\s*:\|:\s*&\s*\};\s*:/.test(segment)) return true;

  // Power/reboot commands
  if (/^\s*(shutdown|reboot|halt|poweroff)\b/.test(segment)) return true;

  // Disk/filesystem destroyers
  if (/^\s*mkfs\b/.test(segment)) return true;
  if (/^\s*fdisk\b/.test(segment)) return true;
  if (/^\s*diskpart\b/.test(segment)) return true;
  if (/^\s*format\s+\S/.test(segment)) return true;
  if (/\bdd\s+.*\sof=\/dev\//.test(segment)) return true;

  // Overwriting critical system files or raw devices
  if (/>\s*\/etc\/(passwd|shadow|sudoers)/.test(segment)) return true;
  if (/>\s*\/dev\/(sda|nvme|hd|sd|mmcblk)/.test(segment)) return true;

  // Windows destructive recursive deletes
  if (/^\s*(del|erase)\s+\/f[\/\s]/i.test(segment)) return true;
  if (/^\s*rd\s+\/s\s+\/q\b/i.test(segment)) return true;

  // rm -rf targeting root or wildcards
  const tokens = segment.split(/\s+/);
  if (tokens[0] === "rm" || (tokens[1] === "rm" && tokens[1])) {
    const hasRecursiveForce = tokens.slice(1).some(
      (t) => /^-/.test(t) && t.includes("r") && t.includes("f")
    );
    if (hasRecursiveForce) {
      for (const arg of tokens.slice(1)) {
        if (arg.startsWith("-")) continue;
        if (arg === "/" || arg === "/*" || arg === "/.*" || arg === "*" || arg === "./*") {
          return true;
        }
      }
    }
  }

  return false;
}
