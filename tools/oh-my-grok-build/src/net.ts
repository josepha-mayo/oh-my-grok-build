import { hostname } from "node:os";
import { isIPv4, isIPv6 } from "node:net";
import { lookup } from "node:dns/promises";

const CLOUD_METADATA_HOSTS = new Set(["metadata.google.internal", "169.254.169.254"]);
const PRIVATE_HOSTS = new Set(["localhost", "127.0.0.1", "::1", "0.0.0.0"]);
const LOOPBACK_HOSTS = new Set(["localhost", "127.0.0.1", "::1"]);

function normalizeHost(raw: string): string {
  let host = raw.toLowerCase();
  if (host.startsWith("[") && host.endsWith("]")) {
    host = host.slice(1, -1);
  } else {
    // Remove trailing dot(s) from FQDNs. localhost. and metadata.google.internal.
    // resolve the same as their non-dotted forms and are a common SSRF bypass.
    host = host.replace(/\.+$/, "");
  }
  return host;
}

function parseIpv4Mapped(tail: string): string | undefined {
  if (tail.includes(".")) return tail;
  const parts = tail.split(":").filter(Boolean);
  if (parts.length === 0 || parts.length > 2) return undefined;
  const full = parts.map((p) => p.padStart(4, "0")).join("");
  if (!/^[0-9a-fA-F]{8}$/.test(full)) return undefined;
  const n = parseInt(full, 16);
  return `${(n >>> 24) & 0xff}.${(n >>> 16) & 0xff}.${(n >>> 8) & 0xff}.${n & 0xff}`;
}

export function isPrivateIp(ip: string): boolean {
  if (!isIPv4(ip) && !isIPv6(ip)) return false;
  if (ip === "0.0.0.0") return true;
  if (ip.startsWith("::ffff:")) {
    const mapped = parseIpv4Mapped(ip.slice(7));
    // If we cannot parse the IPv4-mapped form, block it to be safe.
    return mapped ? isPrivateIp(mapped) : true;
  }
  if (isIPv4(ip) && (ip === "127.0.0.1" || ip.startsWith("127."))) return true;
  if (ip === "::1" || ip === "::") return true;
  if (isIPv4(ip) && (ip.startsWith("10.") || ip.startsWith("192.168."))) return true;
  if (isIPv4(ip) && /^172\.(1[6-9]|2[0-9]|3[0-1])\./.test(ip)) return true;
  if (isIPv4(ip) && ip.startsWith("169.254.")) return true;
  if (isIPv6(ip) && (ip.startsWith("fc") || ip.startsWith("fd"))) return true;
  if (isIPv6(ip) && /^fe[89ab][0-9a-f]:/i.test(ip)) return true;
  return false;
}

export function isLoopbackHost(host: string): boolean {
  const h = normalizeHost(host);
  if (LOOPBACK_HOSTS.has(h)) return true;
  if (isIPv4(h) && h.startsWith("127.")) return true;
  if (h === "::1") return true;
  if (h.startsWith("::ffff:")) {
    const mapped = parseIpv4Mapped(h.slice(7));
    if (mapped) return isLoopbackHost(mapped);
    return false;
  }
  if (isIPv6(h) && !h.includes(".")) {
    // Only treat compressed/expanded ::1 and IPv4-mapped (handled above) as loopback.
    return h === "::1";
  }
  return false;
}

export function isAllowedHttpUrl(raw: string): { ok: true } | { ok: false; reason: string } {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { ok: false, reason: "Invalid URL" };
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    return { ok: false, reason: `Blocked non-HTTP(S) protocol: ${url.protocol}` };
  }
  const host = normalizeHost(url.hostname);
  if (PRIVATE_HOSTS.has(host) || CLOUD_METADATA_HOSTS.has(host)) {
    return { ok: false, reason: "Blocked local/private/metadata host" };
  }
  if (host === hostname().toLowerCase()) {
    return { ok: false, reason: "Blocked local machine hostname" };
  }
  if (isPrivateIp(host)) {
    return { ok: false, reason: "Blocked private IP address" };
  }
  if (url.username || url.password) {
    return { ok: false, reason: "URLs with embedded credentials are not allowed" };
  }
  return { ok: true };
}

export function isAllowedWsUrl(raw: string, allowPrivate = false): { ok: true } | { ok: false; reason: string } {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { ok: false, reason: "Invalid WebSocket URL" };
  }
  if (url.protocol !== "ws:" && url.protocol !== "wss:") {
    return { ok: false, reason: `Blocked non-WS(S) protocol: ${url.protocol}` };
  }
  const host = normalizeHost(url.hostname);
  if (CLOUD_METADATA_HOSTS.has(host)) {
    return { ok: false, reason: "Blocked cloud metadata host" };
  }
  if (host.startsWith("169.254.")) {
    return { ok: false, reason: "Blocked link-local IP address" };
  }
  if (host === "0.0.0.0") {
    return { ok: false, reason: "Blocked broadcast address" };
  }
  if (!allowPrivate && !LOOPBACK_HOSTS.has(host) && isPrivateIp(host)) {
    return { ok: false, reason: "Blocked private IP address (use --bind 127.0.0.1 or pass an explicit loopback URL)" };
  }
  if (url.username || url.password) {
    return { ok: false, reason: "URLs with embedded credentials are not allowed" };
  }
  return { ok: true };
}

function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  return Promise.race([
    promise,
    new Promise<never>((_, reject) => setTimeout(() => reject(new Error("DNS lookup timed out")), ms)),
  ]);
}

export async function isAllowedProviderUrl(raw: string): Promise<{ ok: true } | { ok: false; reason: string }> {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { ok: false, reason: "Invalid URL" };
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    return { ok: false, reason: `Blocked non-HTTP(S) protocol: ${url.protocol}` };
  }
  const host = normalizeHost(url.hostname);
  if (CLOUD_METADATA_HOSTS.has(host)) {
    return { ok: false, reason: "Blocked cloud metadata host" };
  }
  if (isLoopbackHost(host)) return { ok: true };
  if (host === hostname().toLowerCase()) {
    return { ok: false, reason: "Blocked local machine hostname" };
  }
  if (isPrivateIp(host)) {
    return { ok: false, reason: "Blocked private IP address" };
  }
  if (url.username || url.password) {
    return { ok: false, reason: "URLs with embedded credentials are not allowed" };
  }

  // Resolve hostnames to mitigate DNS-rebinding SSRF attempts. IP literals are already classified above.
  if (!isIPv4(host) && !isIPv6(host)) {
    try {
      const addresses = await withTimeout(lookup(host, { all: true }), 5000);
      for (const { address } of addresses) {
        const a = normalizeHost(address);
        if (isLoopbackHost(a)) continue;
        if (isPrivateIp(a)) {
          return { ok: false, reason: `Blocked private IP address resolved from ${host}` };
        }
        if (CLOUD_METADATA_HOSTS.has(a)) {
          return { ok: false, reason: `Blocked cloud metadata host resolved from ${host}` };
        }
      }
    } catch (err) {
      const code = (err as { code?: string }).code;
      if (code === "ENOTFOUND") {
        // Allow unresolvable hostnames to fail later at request time; many local-only names are not in DNS.
        return { ok: true };
      }
      return { ok: false, reason: `DNS lookup failed for ${host}: ${code ?? String(err)}` };
    }
  }

  return { ok: true };
}
