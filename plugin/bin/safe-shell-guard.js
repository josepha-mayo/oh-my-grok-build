#!/usr/bin/env node
const input = [];
process.stdin.on("data", (d) => input.push(d));
process.stdin.on("end", () => {
  const payload = JSON.parse(input.join(""));
  const cmd = payload.toolInput?.command ?? "";
  const dangerous = /(^|\s|;|&&|\|\|)(rm\s+-rf\s+\/|mkfs|dd\s+if=|\:\(\)\{ \:|\>\s*\/etc\/passwd|<.*>\/dev\/(sda|nvme))/i;
  if (dangerous.test(cmd)) {
    console.log(JSON.stringify({ decision: "deny", reason: "Blocked potentially destructive command by oh-my-grok-build safe-shell guard" }));
    process.exit(2);
  }
  console.log(JSON.stringify({ decision: "allow" }));
});
