import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { builtInMcpServers, mergeMcpConfigs, toAcpMcpServers } from "./mcp-config.js";

describe("mcp-config", () => {
  it("lists built-in servers disabled by default except memory", () => {
    const servers = builtInMcpServers();
    const names = servers.map((s) => s.name).sort();
    assert.deepEqual(names, ["omgb-browser", "omgb-computer", "omgb-memory"]);
    const memory = servers.find((s) => s.name === "omgb-memory");
    assert.equal(memory?.enabled, true);
    assert.equal(servers.find((s) => s.name === "omgb-browser")?.enabled, false);
  });

  it("merges stored configs over built-in defaults", () => {
    const stored = [{ name: "omgb-browser", enabled: true, command: "node", args: ["browser.js"] }];
    const merged = mergeMcpConfigs(stored);
    const browser = merged.find((s) => s.name === "omgb-browser");
    assert.equal(browser?.enabled, true);
    assert.deepEqual(browser?.args, ["browser.js"]);
  });

  it("converts enabled servers to ACP stdio entries", () => {
    const servers = [
      { name: "omgb-memory", enabled: true, command: "node", args: ["memory.js"] },
      { name: "omgb-browser", enabled: false, command: "node", args: ["browser.js"] },
    ];
    const acp = toAcpMcpServers(servers);
    assert.equal(acp.length, 1);
    assert.equal(acp[0].type, "stdio");
    assert.equal(acp[0].name, "omgb-memory");
    assert.equal(acp[0].command, "node");
    assert.deepEqual(acp[0].args, ["memory.js"]);
  });

  it("includes env vars when present", () => {
    const servers = [{ name: "x", enabled: true, command: "node", args: ["x.js"], env: { FOO: "bar" } }];
    const acp = toAcpMcpServers(servers);
    assert.deepEqual(acp[0].env, [{ name: "FOO", value: "bar" }]);
  });
});
