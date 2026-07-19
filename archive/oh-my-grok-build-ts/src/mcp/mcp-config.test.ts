import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  builtInMcpServers,
  mergeMcpConfigs,
  toAcpMcpServers,
  isBuiltinMcpServer,
  validateMcpServerConfig,
} from "./mcp-config.js";

describe("mcp-config", () => {
  it("lists built-in servers disabled by default except memory", () => {
    const servers = builtInMcpServers();
    const names = servers.map((s) => s.name).sort();
    assert.deepEqual(names, ["omgb-browser", "omgb-computer", "omgb-memory"]);
    const memory = servers.find((s) => s.name === "omgb-memory");
    assert.equal(memory?.enabled, true);
    assert.equal(servers.find((s) => s.name === "omgb-browser")?.enabled, false);
  });

  it("detects built-in servers by name", () => {
    assert.equal(isBuiltinMcpServer("omgb-browser"), true);
    assert.equal(isBuiltinMcpServer("custom"), false);
  });

  it("merges stored enabled flag over built-in defaults but keeps built-in command/args", () => {
    const stored = [{ name: "omgb-browser", enabled: true, command: "node", args: ["browser.js"] }];
    const merged = mergeMcpConfigs(stored);
    const browser = merged.find((s) => s.name === "omgb-browser");
    assert.equal(browser?.enabled, true);
    assert.notDeepEqual(browser?.args, ["browser.js"]);
    assert.ok(browser?.args[0]?.endsWith("browser.js"));
  });

  it("keeps custom servers from stored config", () => {
    const absoluteScript = process.platform === "win32" ? "C:\\path\\to\\my-server.js" : "/path/to/my-server.js";
    const stored = [{ name: "my-server", enabled: true, command: "node", args: [absoluteScript] }];
    const merged = mergeMcpConfigs(stored);
    const custom = merged.find((s) => s.name === "my-server");
    assert.equal(custom?.enabled, true);
    assert.deepEqual(custom?.args, [absoluteScript]);
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

  it("includes API-key env vars when present", () => {
    const servers = [{ name: "x", enabled: true, command: "node", args: ["x.js"], env: { FOO_API_KEY: "bar" } }];
    const acp = toAcpMcpServers(servers);
    assert.deepEqual(acp[0].env, [{ name: "FOO_API_KEY", value: "bar" }]);
  });

  it("filters out dangerous env vars", () => {
    const servers = [
      {
        name: "x",
        enabled: true,
        command: "node",
        args: ["x.js"],
        env: { LD_PRELOAD: "/tmp/evil.so", PATH: "/tmp", FOO_API_KEY: "ok" },
      },
    ];
    const acp = toAcpMcpServers(servers);
    assert.deepEqual(acp[0].env, [{ name: "FOO_API_KEY", value: "ok" }]);
  });

  it("rejects custom servers with dangerous commands", () => {
    assert.throws(() =>
      validateMcpServerConfig({ name: "evil", enabled: true, command: "bash", args: ["-c", "rm -rf /"] })
    );
    assert.throws(() =>
      validateMcpServerConfig({ name: "evil", enabled: true, command: "node", args: ["-e", "code"] })
    );
    assert.throws(() => validateMcpServerConfig({ name: "evil", enabled: true, command: "./server", args: [] }));
    const absoluteScript = process.platform === "win32" ? "C:\\path\\to\\server.js" : "/path/to/server.js";
    assert.throws(() =>
      validateMcpServerConfig({ name: "evil", enabled: true, command: "./node", args: [absoluteScript] })
    );
    assert.throws(() =>
      validateMcpServerConfig({ name: "evil", enabled: true, command: "../node", args: [absoluteScript] })
    );
  });

  it("accepts valid custom server configs", () => {
    const absoluteServer = process.platform === "win32" ? "C:\\server.exe" : "/usr/bin/my-server";
    const absoluteScript = process.platform === "win32" ? "C:\\path\\to\\server.js" : "/path/to/server.js";
    const absoluteInterpreter = process.platform === "win32" ? "C:\\Program Files\\nodejs\\node.exe" : "/usr/bin/node";
    assert.doesNotThrow(() =>
      validateMcpServerConfig({ name: "my", enabled: true, command: "node", args: [absoluteScript] })
    );
    assert.doesNotThrow(() =>
      validateMcpServerConfig({ name: "my", enabled: true, command: absoluteServer, args: ["--port", "8080"] })
    );
    assert.doesNotThrow(() =>
      validateMcpServerConfig({
        name: "my",
        enabled: true,
        command: absoluteInterpreter,
        args: [absoluteScript],
      })
    );
  });
});
