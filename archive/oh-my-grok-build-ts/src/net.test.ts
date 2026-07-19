import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  isAllowedHttpUrl,
  isAllowedProviderUrl,
  isAllowedWsUrl,
  isLoopbackHost,
  isPrivateIp,
  lookupFromAddresses,
  resolveProviderUrl,
} from "./net.js";
import type { LookupAddress } from "node:dns";

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
      // Full-form IPv6 loopback and unspecified addresses.
      assert.strictEqual(isPrivateIp("0:0:0:0:0:0:0:1"), true);
      assert.strictEqual(isPrivateIp("0:0:0:0:0:0:0:0"), true);
      // Deprecated IPv4-compatible forms are still private/loopback.
      assert.strictEqual(isPrivateIp("::7f00:1"), true);
      assert.strictEqual(isPrivateIp("::c0a8:101"), true);
      assert.strictEqual(isPrivateIp("[::1]"), true);
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
      // Deprecated IPv4-compatible forms are also loopback/private.
      assert.strictEqual(isAllowedHttpUrl("http://[::7f00:1]:8080").ok, false);
      assert.strictEqual(isAllowedHttpUrl("http://[::c0a8:101]:8080").ok, false);
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
    it("allows local loopback WebSocket servers", async () => {
      assert.strictEqual((await isAllowedWsUrl("ws://127.0.0.1:7331/acp")).ok, true);
      assert.strictEqual((await isAllowedWsUrl("ws://localhost:8080")).ok, true);
    });

    it("blocks non-WS(S) protocols", async () => {
      assert.strictEqual((await isAllowedWsUrl("http://example.com")).ok, false);
    });

    it("blocks cloud metadata and link-local endpoints", async () => {
      assert.strictEqual((await isAllowedWsUrl("ws://169.254.169.254")).ok, false);
      assert.strictEqual((await isAllowedWsUrl("ws://metadata.google.internal")).ok, false);
      assert.strictEqual((await isAllowedWsUrl("ws://metadata.google.internal.")).ok, false);
    });

    it("blocks RFC1918 private addresses by default", async () => {
      assert.strictEqual((await isAllowedWsUrl("ws://10.0.0.1")).ok, false);
      assert.strictEqual((await isAllowedWsUrl("ws://192.168.1.1")).ok, false);
      assert.strictEqual((await isAllowedWsUrl("ws://172.16.0.1")).ok, false);
      // Deprecated IPv4-compatible private forms are still private.
      assert.strictEqual((await isAllowedWsUrl("ws://[::c0a8:101]")).ok, false);
    });

    it("allows RFC1918 private addresses when explicitly permitted", async () => {
      assert.strictEqual((await isAllowedWsUrl("ws://10.0.0.1", true)).ok, true);
      assert.strictEqual((await isAllowedWsUrl("ws://192.168.1.1", true)).ok, true);
      assert.strictEqual((await isAllowedWsUrl("ws://172.16.0.1", true)).ok, true);
      assert.strictEqual((await isAllowedWsUrl("ws://[::c0a8:101]", true)).ok, true);
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
      // Full-form IPv6 loopback is still loopback.
      assert.strictEqual(isLoopbackHost("0:0:0:0:0:0:0:1"), true);
      // Deprecated IPv4-compatible loopback forms are still loopback.
      assert.strictEqual(isLoopbackHost("::7f00:1"), true);
    });

    it("rejects non-loopback hosts", () => {
      assert.strictEqual(isLoopbackHost("localhost.evil.com"), false);
      assert.strictEqual(isLoopbackHost("127.foo.com"), false);
      assert.strictEqual(isLoopbackHost("192.168.1.1"), false);
      assert.strictEqual(isLoopbackHost("10.0.0.1"), false);
      assert.strictEqual(isLoopbackHost("example.com"), false);
      assert.strictEqual(isLoopbackHost("::ffff:192.168.1.1"), false);
      // Link-local IPv6 is not loopback.
      assert.strictEqual(isLoopbackHost("fe80:0:0:0:0:0:0:1"), false);
      // Deprecated IPv4-compatible private forms are not loopback.
      assert.strictEqual(isLoopbackHost("::c0a8:101"), false);
      assert.strictEqual(isLoopbackHost("::"), false);
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
      // Deprecated IPv4-compatible loopback forms are still loopback.
      assert.strictEqual((await isAllowedProviderUrl("http://[::7f00:1]:8080/v1")).ok, true);
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
      // Deprecated IPv4-compatible private forms are still private.
      assert.strictEqual((await isAllowedProviderUrl("http://[::c0a8:101]:8000/v1")).ok, false);
    });

    it("blocks URLs with embedded credentials", async () => {
      assert.strictEqual((await isAllowedProviderUrl("https://user:pass@example.com")).ok, false);
    });
  });

  describe("lookupFromAddresses", () => {
    const addresses: LookupAddress[] = [
      { address: "127.0.0.1", family: 4 },
      { address: "::1", family: 6 },
    ];

    it("returns all addresses when options.all is true", () => {
      const lookup = lookupFromAddresses(addresses);
      return new Promise<void>((resolve, reject) => {
        lookup("localhost", { all: true }, (err, result) => {
          if (err) return reject(err);
          assert.deepStrictEqual(result, addresses);
          resolve();
        });
      });
    });

    it("returns the first address and family when options.all is false", () => {
      const lookup = lookupFromAddresses(addresses);
      return new Promise<void>((resolve, reject) => {
        lookup("localhost", { all: false }, (err, address, family) => {
          if (err) return reject(err);
          assert.strictEqual(address, "127.0.0.1");
          assert.strictEqual(family, 4);
          resolve();
        });
      });
    });

    it("filters by family when options.family is set", () => {
      const lookup = lookupFromAddresses(addresses);
      return new Promise<void>((resolve, reject) => {
        lookup("localhost", { family: 6 }, (err, address, family) => {
          if (err) return reject(err);
          assert.strictEqual(address, "::1");
          assert.strictEqual(family, 6);
          resolve();
        });
      });
    });

    it("filters by family when family is a string", () => {
      const lookup = lookupFromAddresses(addresses);
      return new Promise<void>((resolve, reject) => {
        lookup("localhost", { family: "IPv6" }, (err, address, family) => {
          if (err) return reject(err);
          assert.strictEqual(address, "::1");
          assert.strictEqual(family, 6);
          resolve();
        });
      });
    });

    it("returns all matching addresses when family and all are set", () => {
      const lookup = lookupFromAddresses(addresses);
      return new Promise<void>((resolve, reject) => {
        lookup("localhost", { all: true, family: 4 }, (err, result) => {
          if (err) return reject(err);
          assert.deepStrictEqual(result, [{ address: "127.0.0.1", family: 4 }]);
          resolve();
        });
      });
    });

    it("errors when no address matches the requested family", () => {
      const lookup = lookupFromAddresses([{ address: "127.0.0.1", family: 4 }]);
      return new Promise<void>((resolve, reject) => {
        lookup("localhost", { family: 6 }, (err, address, family) => {
          if (!err) return reject(new Error("expected an error"));
          assert.strictEqual((err as { code?: string }).code, "ENOTFOUND");
          resolve();
        });
      });
    });
  });

  describe("resolveProviderUrl", () => {
    it("does not return a lookup for IP literals", async () => {
      const result = await resolveProviderUrl("http://127.0.0.1:8000/v1");
      assert.strictEqual(result.ok, true);
      if (!result.ok) return;
      assert.strictEqual(result.lookup, undefined);
    });

    it("resolves localhost to a pinned loopback lookup", { timeout: 10000 }, async () => {
      const result = await resolveProviderUrl("http://localhost:11434/v1");
      assert.strictEqual(result.ok, true);
      if (!result.ok) return;
      assert.ok(result.lookup, "expected a lookup for localhost");
      const addresses = await new Promise<LookupAddress[]>((resolve, reject) => {
        result.lookup!("localhost", { all: true }, (err, addrs) => {
          if (err) return reject(err);
          resolve(addrs as LookupAddress[]);
        });
      });
      assert.ok(addresses.length > 0, "expected at least one localhost address");
      assert.ok(
        addresses.every((a) => isLoopbackHost(a.address)),
        "expected only loopback addresses"
      );
    });

    it("rejects unresolvable hostnames", { timeout: 10000 }, async () => {
      const result = await resolveProviderUrl("http://omgb-test-does-not-resolve.invalid:8000/v1");
      assert.strictEqual(result.ok, false);
      if (result.ok) return;
      assert.ok(result.reason.includes("DNS lookup failed"), `unexpected reason: ${result.reason}`);
    });

    it("treats deprecated IPv4-compatible loopback as loopback", async () => {
      const result = await resolveProviderUrl("http://[::7f00:1]:8080/v1");
      assert.strictEqual(result.ok, true);
      if (!result.ok) return;
      assert.strictEqual(result.lookup, undefined);
    });

    it("blocks deprecated IPv4-compatible private IPs", async () => {
      const result = await resolveProviderUrl("http://[::c0a8:101]:8080/v1");
      assert.strictEqual(result.ok, false);
    });
  });
});
