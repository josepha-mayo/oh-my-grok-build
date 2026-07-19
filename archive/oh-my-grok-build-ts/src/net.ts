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

function groupsToIpv4(high: string, low: string): string {
  const h = (parseInt(high, 16) || 0) >>> 0;
  const l = (parseInt(low, 16) || 0) >>> 0;
  return `${(h >>> 8) & 0xff}.${h & 0xff}.${(l >>> 8) & 0xff}.${l & 0xff}`;
}

function expandIpv6(ip: string): string[] | undefined {
  ip = normalizeHost(ip).toLowerCase();
  const groups = ip.split(":");
  const last = groups[groups.length - 1];
  if (last.includes(".")) {
    if (!isIPv4(last)) return undefined;
    const [a, b, c, d] = last.split(".").map((n) => parseInt(n, 10));
    const high = (((a << 8) | b) >>> 0).toString(16);
    const low = (((c << 8) | d) >>> 0).toString(16);
    groups[groups.length - 1] = high;
    groups.push(low);
  }

  const joined = groups.join(":");
  const parts = joined.split("::");
  if (parts.length > 2) return undefined;
  const left = parts[0] ? parts[0].split(":") : [];
  const right = parts[1] ? parts[1].split(":") : [];
  const missing = 8 - left.length - right.length;
  if (missing < 0) return undefined;
  const expanded = [...left, ...new Array(missing).fill("0"), ...right].map((g) =>
    ((parseInt(g || "0", 16) || 0) >>> 0).toString(16)
  );
  if (expanded.length !== 8) return undefined;
  return expanded;
}

export function isPrivateIp(ip: string): boolean {
  ip = normalizeHost(ip);
  if (!isIPv4(ip) && !isIPv6(ip)) return false;
  if (isIPv4(ip)) {
    if (ip === "0.0.0.0") return true;
    if (ip === "127.0.0.1" || ip.startsWith("127.")) return true;
    if (ip.startsWith("10.") || ip.startsWith("192.168.")) return true;
    if (/^172\.(1[6-9]|2[0-9]|3[0-1])\./.test(ip)) return true;
    if (ip.startsWith("169.254.")) return true;
    return false;
  }

  const expanded = expandIpv6(ip);
  if (!expanded) return false;
  if (expanded.every((g) => g === "0")) return true;
  if (expanded.every((g, i) => (i < 7 ? g === "0" : g === "1"))) return true;

  const isMapped = expanded.slice(0, 5).every((g) => g === "0") && expanded[5] === "ffff";
  const isCompatible = expanded.slice(0, 6).every((g) => g === "0");
  if (isMapped || isCompatible) {
    return isPrivateIp(groupsToIpv4(expanded[6], expanded[7]));
  }

  if (expanded[0].startsWith("fc") || expanded[0].startsWith("fd")) return true;
  if (/^fe[89ab][0-9a-f]$/i.test(expanded[0])) return true;
  return false;
}

export function isLoopbackHost(host: string): boolean {
  const h = normalizeHost(host);
  if (LOOPBACK_HOSTS.has(h)) return true;
  if (isIPv4(h) && h.startsWith("127.")) return true;
  if (!isIPv6(h)) return false;

  const expanded = expandIpv6(h);
  if (!expanded) return false;
  if (expanded.every((g, i) => (i < 7 ? g === "0" : g === "1"))) return true;

  const isMapped = expanded.slice(0, 5).every((g) => g === "0") && expanded[5] === "ffff";
  const isCompatible = expanded.slice(0, 6).every((g) => g === "0");
  if (isMapped || isCompatible) {
    return isLoopbackHost(groupsToIpv4(expanded[6], expanded[7]));
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

export function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
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
