import type { AcpPermissionOption, AcpPermissionResponse } from "./types.js";

const ALLOW_KINDS = ["allow", "approve"];
const ALLOW_ONCE_KINDS = ["allow_once", "allow-once", "approve_once"];
const ALLOW_ALWAYS_KINDS = ["allow_always", "allow-always", "approve_always"];

function matchesKind(o: AcpPermissionOption, kinds: string[]): boolean {
  const kind = (o.kind ?? "").toLowerCase();
  const optionId = (o.optionId ?? "").toLowerCase();
  for (const k of kinds) {
    if (kind === k || kind.startsWith(`${k}_`) || kind.startsWith(`${k}-`)) return true;
    // Match whole words / delimited forms of the kind in the option id.
    const re = new RegExp(`(^|[-_.])${k}($|[-_.])`, "i");
    if (re.test(optionId)) return true;
  }
  return false;
}

/**
 * Select an allow-style permission option.
 *
 * In yolo mode, prefer a permanent allow option. Otherwise prefer `allow_once`.
 * If no clear allow option exists, return undefined (cancel).
 * The function never falls back to the first option, so it will not
 * accidentally select a Cancel/Deny choice.
 */
export function selectPermissionOption(options: AcpPermissionOption[], yolo = false): string | undefined {
  if (options.length === 0) return undefined;
  if (yolo) {
    const always = options.find((o) => matchesKind(o, ALLOW_ALWAYS_KINDS));
    if (always) return always.optionId;
  }
  const once = options.find((o) => matchesKind(o, ALLOW_ONCE_KINDS));
  if (once) return once.optionId;
  const anyAllow = options.find((o) => matchesKind(o, ALLOW_KINDS));
  return anyAllow?.optionId;
}

export function makePermissionResponse(optionId: string | undefined): AcpPermissionResponse {
  return optionId ? { outcome: { outcome: "selected", optionId } } : { outcome: { outcome: "cancelled" } };
}
