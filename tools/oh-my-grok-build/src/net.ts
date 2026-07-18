import { hostname } from "node:os";
import { isIPv4, isIPv6 } from "node:net";
import { lookup } from "node:dns/promises";
import type { LookupAddress } from "node:dns";
import type { LookupFunction } from "node:net";

const CLOUD_METADATA_HOSTS = new Set(["metadata.google.internal", "169.254.169.254"]);
const PRIVATE_HOSTS = new Set(["localhost", "127.0.0.1", "::1", "0.0.0.0"]);
const LOOPBACK_HOSTS = new Set(["localhost", "127.0.0.1", "::1"]);

export type LookupFn = LookupFunction;

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

function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("DNS lookup timed out")), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (err) => {
        clearTimeout(timer);
        reject(err);
      }
    );
  });
}

interface ResolveHostRules {
  allowLoopback: boolean;
  allowPrivate: boolean;
  allowMetadata: boolean;
}

async function resolveHost(
  host: string,
  rules: ResolveHostRules
): Promise<{ ok: true; addresses: LookupAddress[] } | { ok: false; reason: string }> {
  try {
    const result = (await withTimeout(lookup(host, { all: true }), 5000)) as LookupAddress[];
    const addresses: LookupAddress[] = [];
    for (const { address, family } of result) {
      const a = normalizeHost(address);
      if (!rules.allowMetadata && CLOUD_METADATA_HOSTS.has(a)) {
        return { ok: false, reason: `Blocked cloud metadata host resolved from ${host}` };
      }
      if (isLoopbackHost(a)) {
        if (rules.allowLoopback) {
          addresses.push({ address: a, family });
        } else {
          return { ok: false, reason: `Blocked loopback address resolved from ${host}` };
        }
        continue;
      }
      if (isPrivateIp(a)) {
        if (rules.allowPrivate) {
          addresses.push({ address: a, family });
        } else {
          return { ok: false, reason: `Blocked private IP address resolved from ${host}` };
        }
        continue;
      }
      addresses.push({ address: a, family });
    }
    if (addresses.length === 0) {
      return { ok: false, reason: `No usable addresses resolved from ${host}` };
    }
    return { ok: true, addresses };
  } catch (err) {
    const code = (err as { code?: string }).code;
    return { ok: false, reason: `DNS lookup failed for ${host}: ${code ?? String(err)}` };
  }
}

function normalizeFamily(family: unknown): number {
  if (family === "IPv4") return 4;
  if (family === "IPv6") return 6;
  if (typeof family === "number") return family;
  return 0;
}

function normalizeLookupOptions(options: unknown): { all: boolean; family: number } {
  if (options && typeof options === "object") {
    const opts = options as { all?: unknown; family?: unknown };
    return { all: Boolean(opts.all), family: normalizeFamily(opts.family) };
  }
  return { all: false, family: 0 };
}

function filterAddresses(addresses: LookupAddress[], family: number): LookupAddress[] {
  if (family === 0) {
    return addresses;
  }
  return addresses.filter((a) => a.family === family);
}

export function lookupFromAddresses(addresses: LookupAddress[]): LookupFn {
  return (_hostname, options, callback) => {
    const { all, family } = normalizeLookupOptions(options);
    const filtered = filterAddresses(addresses, family);
    if (all) {
      callback(null, filtered);
      return;
    }
    if (filtered.length === 0) {
      callback(Object.assign(new Error("No addresses resolved"), { code: "ENOTFOUND" }), "", 0);
      return;
    }
    const first = filtered[0];
    callback(null, first.address, first.family);
  };
}

export interface ResolvedUrl {
  ok: true;
  url: URL;
  host: string;
  lookup?: LookupFn;
}

interface ResolveUrlOptions {
  protocols: Set<string>;
  allowLoopback: boolean;
  allowPrivate: boolean;
  allowMetadata: boolean;
}

async function resolveUrl(
  raw: string,
  options: ResolveUrlOptions
): Promise<ResolvedUrl | { ok: false; reason: string }> {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { ok: false, reason: "Invalid URL" };
  }
  if (!options.protocols.has(url.protocol)) {
    return { ok: false, reason: `Blocked unsupported protocol: ${url.protocol}` };
  }
  const host = normalizeHost(url.hostname);
  if (!options.allowMetadata && CLOUD_METADATA_HOSTS.has(host)) {
    return { ok: false, reason: "Blocked cloud metadata host" };
  }
  if (host === "0.0.0.0") {
    return { ok: false, reason: "Blocked broadcast address" };
  }
  if (!options.allowMetadata && host.startsWith("169.254.")) {
    return { ok: false, reason: "Blocked link-local IP address" };
  }
  if (host === hostname().toLowerCase()) {
    return { ok: false, reason: "Blocked local machine hostname" };
  }
  if (!options.allowLoopback && isLoopbackHost(host)) {
    return { ok: false, reason: "Blocked loopback host" };
  }
  if (!options.allowPrivate && isPrivateIp(host) && !isLoopbackHost(host)) {
    return { ok: false, reason: "Blocked private IP address" };
  }
  if (url.username || url.password) {
    return { ok: false, reason: "URLs with embedded credentials are not allowed" };
  }
  if (!isIPv4(host) && !isIPv6(host)) {
    const resolved = await resolveHost(host, {
      allowLoopback: options.allowLoopback,
      allowPrivate: options.allowPrivate,
      allowMetadata: options.allowMetadata,
    });
    if (!resolved.ok) return { ok: false, reason: resolved.reason };
    if (resolved.addresses.length > 0) {
      return { ok: true, url, host, lookup: lookupFromAddresses(resolved.addresses) };
    }
  }
  return { ok: true, url, host };
}

export async function resolveProviderUrl(raw: string): Promise<ResolvedUrl | { ok: false; reason: string }> {
  return resolveUrl(raw, {
    protocols: new Set(["http:", "https:"]),
    allowLoopback: true,
    allowPrivate: false,
    allowMetadata: false,
  });
}

export async function isAllowedProviderUrl(raw: string): Promise<{ ok: true } | { ok: false; reason: string }> {
  const result = await resolveProviderUrl(raw);
  return result.ok ? { ok: true } : { ok: false, reason: result.reason };
}

export async function resolveWsUrl(
  raw: string,
  allowPrivate = false
): Promise<ResolvedUrl | { ok: false; reason: string }> {
  return resolveUrl(raw, {
    protocols: new Set(["ws:", "wss:"]),
    allowLoopback: true,
    allowPrivate,
    allowMetadata: false,
  });
}

export async function isAllowedWsUrl(
  raw: string,
  allowPrivate = false
): Promise<{ ok: true } | { ok: false; reason: string }> {
  const result = await resolveWsUrl(raw, allowPrivate);
  return result.ok ? { ok: true } : { ok: false, reason: result.reason };
}

export async function createWsLookup(raw: string, allowPrivate = false): Promise<LookupFn | undefined> {
  const result = await resolveWsUrl(raw, allowPrivate);
  if (!result.ok) throw new Error(result.reason);
  return result.lookup;
}
