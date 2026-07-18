import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { isAllowedHttpUrl, isAllowedProviderUrl, isAllowedWsUrl, isLoopbackHost, isPrivateIp } from "./net.js";

describe("net", () => {
  describe("isPrivateIp", () => {
    it("detects loopback addresses", () => {
      assert.strictEqual(isPrivateIp("127.0.0.1"), true);
      assert.strictEqual(isPrivateIp("127.0.0.2"), true);
      assert.strictEqual(isPrivateIp("127.255.255.255"), true);
      assert.strictEqual(isPrivateIp("::1"), true);
      assert.strictEqual(isPrivateIp("::"), true);
      assert.strictEqual(isPrivateIp("::ffff:127.0.0.1"), true);
      // Node URL normalizes IPv4-mapped loopback to the compressed hex form.
      assert.strictEqual(isPrivateIp("::ffff:7f00:1"), true);
      assert.strictEqual(isPrivateIp("::ffff:7f00:2"), true);
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

    it("does not treat IP-looking hostnames as addresses", () => {
      assert.strictEqual(isPrivateIp("127.foo.com"), false);
      assert.strictEqual(isPrivateIp("10.0.0.1.nip.io"), false);
      assert.strictEqual(isPrivateIp("192.168.1.1.evil.com"), false);
      assert.strictEqual(isPrivateIp("fe80.example.com"), false);
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
      assert.strictEqual(isAllowedHttpUrl("http://127.0.0.2:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://[::1]:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://[::]:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://[::ffff:127.0.0.1]:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://[::ffff:127.0.0.2]:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://[::ffff:7f00:1]:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://[::ffff:7f00:2]:8080").ok, false);
      // Node normalizes dotted octal/hex loopback forms to 127.0.0.1.
      assert.strictEqual(isAllowedHttpUrl("http://0177.0.0.1:8080").ok, false);
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

  describe("isLoopbackHost", () => {
    it("detects loopback hosts", () => {
      assert.strictEqual(isLoopbackHost("localhost"), true);
      assert.strictEqual(isLoopbackHost("127.0.0.1"), true);
      assert.strictEqual(isLoopbackHost("127.0.0.2"), true);
      assert.strictEqual(isLoopbackHost("[::1]"), true);
      assert.strictEqual(isLoopbackHost("::1"), true);
      assert.strictEqual(isLoopbackHost("::ffff:127.0.0.1"), true);
      assert.strictEqual(isLoopbackHost("::ffff:7f00:1"), true);
      assert.strictEqual(isLoopbackHost("localhost."), true);
    });

    it("rejects non-loopback hosts", () => {
      assert.strictEqual(isLoopbackHost("localhost.evil.com"), false);
      assert.strictEqual(isLoopbackHost("127.foo.com"), false);
      assert.strictEqual(isLoopbackHost("192.168.1.1"), false);
      assert.strictEqual(isLoopbackHost("10.0.0.1"), false);
      assert.strictEqual(isLoopbackHost("example.com"), false);
      assert.strictEqual(isLoopbackHost("::ffff:192.168.1.1"), false);
    });
  });

  describe("isAllowedProviderUrl", () => {
    it("allows public HTTPS URLs", async () => {
      const result = await isAllowedProviderUrl("https://api.openai.com/v1");
      assert.strictEqual(result.ok, true);
    });

    it("allows loopback URLs", async () => {
      assert.strictEqual((await isAllowedProviderUrl("http://localhost:11434/v1")).ok, true);
      assert.strictEqual((await isAllowedProviderUrl("http://127.0.0.1:8000/v1")).ok, true);
      assert.strictEqual((await isAllowedProviderUrl("http://[::1]:8080/v1")).ok, true);
    });

    it("blocks non-HTTP(S) protocols", async () => {
      assert.strictEqual((await isAllowedProviderUrl("file:///etc/passwd")).ok, false);
    });

    it("blocks cloud metadata endpoints", async () => {
      assert.strictEqual((await isAllowedProviderUrl("http://169.254.169.254/latest/meta-data/")).ok, false);
      assert.strictEqual((await isAllowedProviderUrl("http://metadata.google.internal")).ok, false);
    });

    it("blocks non-loopback private IPs", async () => {
      assert.strictEqual((await isAllowedProviderUrl("http://192.168.1.1:8000/v1")).ok, false);
      assert.strictEqual((await isAllowedProviderUrl("http://10.0.0.1:8000/v1")).ok, false);
      assert.strictEqual((await isAllowedProviderUrl("http://172.16.0.1:8000/v1")).ok, false);
    });

    it("blocks URLs with embedded credentials", async () => {
      assert.strictEqual((await isAllowedProviderUrl("https://user:pass@example.com")).ok, false);
    });
  });
});
