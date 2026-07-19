import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { addConnector, getConnector, listConnectors, removeConnector, buildConnector } from "./manager.js";

let tempDir: string;

describe("integrations manager", () => {
  beforeEach(() => {
    tempDir = mkdtempSync(join(tmpdir(), "omgb-integrations-"));
    process.env.OMGB_HOME = tempDir;
  });

  afterEach(() => {
    delete process.env.OMGB_HOME;
    rmSync(tempDir, { recursive: true, force: true });
  });

  it("adds and lists connectors", async () => {
    await addConnector({ name: "codex-main", type: "codex", cwd: "/tmp" });
    const list = await listConnectors();
    assert.strictEqual(list.length, 1);
    assert.strictEqual(list[0].name, "codex-main");
  });

  it("removes a connector", async () => {
    await addConnector({ name: "x", type: "claude" });
    await removeConnector("x");
    assert.strictEqual((await listConnectors()).length, 0);
  });

  it("builds a connector of the correct type", async () => {
    const cfg = { name: "o", type: "opencode" as const, url: "ws://localhost:1/acp" };
    await addConnector(cfg);
    const loaded = await getConnector("o");
    assert.ok(loaded);
    const c = buildConnector(loaded!);
    assert.strictEqual(c.config.type, "opencode");
    assert.ok(c.close);
  });

  it("builds hermes, pi, and omp connectors", async () => {
    const hermes = buildConnector({ name: "h", type: "hermes" });
    assert.strictEqual(hermes.config.type, "hermes");
    assert.ok(hermes.close);

    const pi = buildConnector({ name: "p", type: "pi" });
    assert.strictEqual(pi.config.type, "pi");

    const omp = buildConnector({ name: "m", type: "omp" });
    assert.strictEqual(omp.config.type, "omp");
    assert.ok(omp.close);
  });
});
