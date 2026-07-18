import { hostname } from "node:os";

const CLOUD_METADATA_HOSTS = new Set(["metadata.google.internal", "169.254.169.254"]);
const PRIVATE_HOSTS = new Set(["localhost", "127.0.0.1", "::1", "0.0.0.0"]);

export function isPrivateIp(ip: string): boolean {
  if (ip.startsWith("::ffff:")) return isPrivateIp(ip.slice(7));
  if (ip === "127.0.0.1" || ip === "::1") return true;
  if (ip.startsWith("10.") || ip.startsWith("192.168.")) return true;
  if (/^172\.(1[6-9]|2[0-9]|3[0-1])\./.test(ip)) return true;
  if (ip.startsWith("169.254.")) return true;
  if (ip.startsWith("fc") || ip.startsWith("fd") || ip.startsWith("fe80:")) return true;
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
  const host = url.hostname.toLowerCase();
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

export function isAllowedWsUrl(raw: string): { ok: true } | { ok: false; reason: string } {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { ok: false, reason: "Invalid WebSocket URL" };
  }
  if (url.protocol !== "ws:" && url.protocol !== "wss:") {
    return { ok: false, reason: `Blocked non-WS(S) protocol: ${url.protocol}` };
  }
  const host = url.hostname.toLowerCase();
  if (CLOUD_METADATA_HOSTS.has(host)) {
    return { ok: false, reason: "Blocked cloud metadata host" };
  }
  if (host.startsWith("169.254.")) {
    return { ok: false, reason: "Blocked link-local IP address" };
  }
  if (url.username || url.password) {
    return { ok: false, reason: "URLs with embedded credentials are not allowed" };
  }
  return { ok: true };
}
