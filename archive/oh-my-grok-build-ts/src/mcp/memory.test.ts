import { describe, it, beforeEach } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { mkdirSync, rmSync } from "node:fs";
import { homedir } from "node:os";

const __dirname = dirname(fileURLToPath(import.meta.url));
const memoryFile = join(homedir(), ".omgb", "memory.json");

function runScript(lines: string[]): Promise<{ code: number | null; output: string }> {
  const script = join(__dirname, "..", "..", "dist", "mcp", "memory.js");
  const proc = spawn("node", [script], { stdio: ["pipe", "pipe", "pipe"] });
  let output = "";
  proc.stdout.on("data", (d) => (output += d.toString()));
  proc.stderr.on("data", (d) => (output += d.toString()));
  for (const line of lines) {
    proc.stdin.write(line + "\n");
  }
  proc.stdin.end();
  return new Promise((resolve) => proc.on("close", (code) => resolve({ code, output })));
}

beforeEach(() => {
  mkdirSync(join(homedir(), ".omgb"), { recursive: true });
  try {
    rmSync(memoryFile, { force: true });
  } catch {}
});

describe("memory MCP server", () => {
  it("returns a list of tools after initialize", async () => {
    const { code, output } = await runScript([
      JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize" }),
      JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list" }),
    ]);
    assert.equal(code, 0);
    const lines = output.split("\n").filter(Boolean);
    const list = JSON.parse(lines[1]);
    assert.equal(list.result.tools.length, 4);
    const names = list.result.tools.map((t: { name: string }) => t.name).sort();
    assert.deepEqual(names, ["memory_forget", "memory_remember", "memory_search", "memory_status"]);
  });

  it("remembers and searches facts", async () => {
    const { code, output } = await runScript([
      JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize" }),
      JSON.stringify({
        jsonrpc: "2.0",
        id: 2,
        method: "tools/call",
        params: { name: "memory_remember", arguments: { content: "I prefer TypeScript", tags: ["preference"] } },
      }),
      JSON.stringify({
        jsonrpc: "2.0",
        id: 3,
        method: "tools/call",
        params: { name: "memory_search", arguments: { query: "TypeScript" } },
      }),
    ]);
    assert.equal(code, 0);
    const lines = output.split("\n").filter(Boolean);
    const search = JSON.parse(lines[2]);
    assert.ok(search.result.content[0].text.includes("TypeScript"));
  });
});
