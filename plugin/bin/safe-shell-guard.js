#!/usr/bin/env node
const input = [];
process.stdin.on("data", (d) => input.push(d));
process.stdin.on("end", () => {
  let payload = {};
  try {
    payload = JSON.parse(input.join(""));
  } catch {
    console.log(
      JSON.stringify({
        decision: "deny",
        reason: "Invalid guard payload",
      })
    );
    process.exit(2);
  }

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
  const segments = splitCommand(command);

  for (const segment of segments) {
    const trimmed = segment.trim();
    if (!trimmed) continue;
    if (isDangerousSegment(trimmed)) return true;
  }

  return false;
}

function splitCommand(command) {
  // Split on common shell separators. This is intentionally simple and can be
  // bypassed by determined input, but it catches the obvious chained cases.
  return command.split(/;|&&|\|\||`|\$\([^)]*\)|\r?\n/);
}

function isDangerousSegment(segment) {
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
  const tokens = segment.split(/\s+/).filter(Boolean);
  const rmIndex = tokens.indexOf("rm");
  if (rmIndex !== -1) {
    const prefix = tokens.slice(0, rmIndex);
    const isSudoRm = prefix.length === 0 || (prefix[0] === "sudo" && prefix.slice(1).every((t) => t.startsWith("-")));
    if (isSudoRm) {
      const flags = new Set();
      let sawDoubleDash = false;
      for (let i = rmIndex + 1; i < tokens.length; i++) {
        const arg = tokens[i];
        if (!sawDoubleDash && arg === "--") {
          sawDoubleDash = true;
          continue;
        }
        if (!sawDoubleDash && arg.startsWith("-")) {
          if (arg.startsWith("--")) {
            const name = arg.slice(2);
            if (name === "recursive" || name === "r" || name === "remove" || name === "R") flags.add("r");
            if (name === "force" || name === "f") flags.add("f");
          } else {
            for (const ch of arg.slice(1)) {
              if (ch === "r" || ch === "R") flags.add("r");
              if (ch === "f") flags.add("f");
            }
          }
          continue;
        }
        if ((flags.has("r") || flags.has("R")) && flags.has("f") && isDangerousRmTarget(arg)) {
          return true;
        }
      }
    }
  }

  return false;
}

function isDangerousRmTarget(arg) {
  const target = arg.replace(/^["']|["']$/g, "");
  if (["/", "/*", "/.*", "*", "./*", "~", "~/"].includes(target)) return true;
  if (/^~\/.*/.test(target)) return true;
  return false;
}
