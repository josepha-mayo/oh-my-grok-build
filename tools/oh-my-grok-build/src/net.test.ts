import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { isAllowedHttpUrl, isAllowedWsUrl, isPrivateIp } from "./net.js";

describe("net", () => {
  describe("isPrivateIp", () => {
    it("detects loopback addresses", () => {
      assert.strictEqual(isPrivateIp("127.0.0.1"), true);
      assert.strictEqual(isPrivateIp("::1"), true);
      assert.strictEqual(isPrivateIp("::ffff:127.0.0.1"), true);
      // Node URL normalizes IPv4-mapped loopback to the compressed hex form.
      assert.strictEqual(isPrivateIp("::ffff:7f00:1"), true);
    });

    it("detects RFC1918 addresses", () => {
      assert.strictEqual(isPrivateIp("10.0.0.1"), true);
      assert.strictEqual(isPrivateIp("192.168.1.1"), true);
      assert.strictEqual(isPrivateIp("172.16.0.1"), true);
      assert.strictEqual(isPrivateIp("172.31.255.255"), true);
      // Compressed IPv4-mapped RFC1918.
      assert.strictEqual(isPrivateIp("::ffff:c0a8:101"), true);
    });

    it("detects link-local and IPv6 link-local", () => {
      assert.strictEqual(isPrivateIp("169.254.169.254"), true);
      assert.strictEqual(isPrivateIp("fe80::1"), true);
      assert.strictEqual(isPrivateIp("febf::1"), true);
    });

    it("returns false for public addresses", () => {
      assert.strictEqual(isPrivateIp("8.8.8.8"), false);
      assert.strictEqual(isPrivateIp("2001:4860:4860::8888"), false);
    });
  });

  describe("isAllowedHttpUrl", () => {
    it("allows public HTTPS URLs", () => {
      const result = isAllowedHttpUrl("https://example.com/path");
      assert.strictEqual(result.ok, true);
    });

    it("blocks non-HTTP(S) protocols", () => {
      const result = isAllowedHttpUrl("file:///etc/passwd");
      assert.strictEqual(result.ok, false);
    });

    it("blocks localhost and loopback", () => {
      assert.strictEqual(isAllowedHttpUrl("http://localhost:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://127.0.0.1:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://[::1]:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://[::ffff:127.0.0.1]:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://[::ffff:7f00:1]:8080").ok, false);
    });

    it("blocks cloud metadata endpoints", () => {
      assert.strictEqual(isAllowedHttpUrl("http://169.254.169.254/latest/meta-data/").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://metadata.google.internal").ok, false);
    });

    it("blocks URLs with embedded credentials", () => {
      assert.strictEqual(isAllowedHttpUrl("https://user:pass@example.com").ok, false);
    });

    it("blocks trailing-dot and percent-encoded FQDN bypasses", () => {
      assert.strictEqual(isAllowedHttpUrl("http://localhost.:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://localhost%2e:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://metadata.google.internal.:8080").ok, false);
    });
  });

  describe("isAllowedWsUrl", () => {
    it("allows local loopback WebSocket servers", () => {
      assert.strictEqual(isAllowedWsUrl("ws://127.0.0.1:7331/acp").ok, true);
      assert.strictEqual(isAllowedWsUrl("ws://localhost:8080").ok, true);
    });

    it("blocks non-WS(S) protocols", () => {
      assert.strictEqual(isAllowedWsUrl("http://example.com").ok, false);
    });

    it("blocks cloud metadata and link-local endpoints", () => {
      assert.strictEqual(isAllowedWsUrl("ws://169.254.169.254").ok, false);
      assert.strictEqual(isAllowedWsUrl("ws://metadata.google.internal").ok, false);
      assert.strictEqual(isAllowedWsUrl("ws://metadata.google.internal.").ok, false);
    });

    it("blocks RFC1918 private addresses by default", () => {
      assert.strictEqual(isAllowedWsUrl("ws://10.0.0.1").ok, false);
      assert.strictEqual(isAllowedWsUrl("ws://192.168.1.1").ok, false);
      assert.strictEqual(isAllowedWsUrl("ws://172.16.0.1").ok, false);
    });

    it("allows RFC1918 private addresses when explicitly permitted", () => {
      assert.strictEqual(isAllowedWsUrl("ws://10.0.0.1", true).ok, true);
      assert.strictEqual(isAllowedWsUrl("ws://192.168.1.1", true).ok, true);
      assert.strictEqual(isAllowedWsUrl("ws://172.16.0.1", true).ok, true);
    });
  });
});
