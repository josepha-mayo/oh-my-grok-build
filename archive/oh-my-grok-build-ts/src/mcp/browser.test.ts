import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { isUrlAllowed, sanitizeAccessibilityRef } from "./browser.js";

describe("browser MCP helpers", () => {
  it("sanitizes valid accessibility refs", () => {
    assert.equal(sanitizeAccessibilityRef("@foo123"), '[data-accessibility-ref="foo123"]');
    assert.equal(sanitizeAccessibilityRef("bar-baz"), '[data-accessibility-ref="bar-baz"]');
  });

  it("rejects invalid accessibility refs", () => {
    assert.throws(() => sanitizeAccessibilityRef("@foo bar"));
    assert.throws(() => sanitizeAccessibilityRef("@foo/../bar"));
    assert.throws(() => sanitizeAccessibilityRef("foo;bar"));
  });

  it("blocks non-HTTP(S) URLs", async () => {
    const result = await isUrlAllowed("file:///etc/passwd");
    assert.equal(result.ok, false);
  });

  it("blocks private IPs after DNS lookup", async () => {
    const fakeLookup = async () => [{ address: "192.168.1.1" }];
    const result = await isUrlAllowed("https://example.com/path", fakeLookup as any);
    assert.equal(result.ok, false);
    assert.ok(String((result as any).reason).includes("private"));
  });

  it("allows public URLs with public resolved IPs", async () => {
    const fakeLookup = async () => [{ address: "1.2.3.4" }];
    const result = await isUrlAllowed("https://example.com/path", fakeLookup as any);
    assert.equal(result.ok, true);
  });

  it("allows URLs with no DNS records (defer to browser)", async () => {
    const fakeLookup = async () => {
      const err = new Error("not found") as any;
      err.code = "ENOTFOUND";
      throw err;
    };
    const result = await isUrlAllowed("https://does-not-exist.example.com", fakeLookup as any);
    assert.equal(result.ok, true);
  });
});
