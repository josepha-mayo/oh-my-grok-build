import { SecureStoragePlugin } from "capacitor-secure-storage-plugin";
import { Capacitor } from "@capacitor/core";

const PERSIST_PREFIX = "omgb:";
const SECURE_PREFIX = "omgb_secure:";

// Non-native (browser preview) storage for secrets. In-memory only so API keys
// are never persisted to browser storage. The native app uses the OS keystore.
const memoryStore = new Map<string, string>();
let memoryFallbackWarned = false;

export function persistGet<T>(key: string): T | null {
  try {
    const raw = localStorage.getItem(`${PERSIST_PREFIX}${key}`);
    if (!raw) return null;
    return JSON.parse(raw) as T;
  } catch {
    return null;
  }
}

export function persistSet(key: string, value: unknown): void {
  try {
    localStorage.setItem(`${PERSIST_PREFIX}${key}`, JSON.stringify(value));
  } catch {
    // ignore
  }
}

export function persistRemove(key: string): void {
  try {
    localStorage.removeItem(`${PERSIST_PREFIX}${key}`);
  } catch {
    // ignore
  }
}

export async function secureGet(key: string): Promise<string | null> {
  const fullKey = `${SECURE_PREFIX}${key}`;
  if (Capacitor.isNativePlatform()) {
    try {
      const result = await SecureStoragePlugin.get({ key: fullKey });
      return result.value ?? null;
    } catch {
      return null;
    }
  }
  if (!memoryFallbackWarned) {
    memoryFallbackWarned = true;
    console.warn("OMGB: non-native platform detected; API keys are kept in memory only and will not persist.");
  }
  return memoryStore.get(fullKey) ?? null;
}

export async function secureSet(key: string, value: string): Promise<void> {
  const fullKey = `${SECURE_PREFIX}${key}`;
  if (Capacitor.isNativePlatform()) {
    try {
      await SecureStoragePlugin.set({ key: fullKey, value });
    } catch {
      // ignore
    }
    return;
  }
  memoryStore.set(fullKey, value);
}

export async function secureRemove(key: string): Promise<void> {
  const fullKey = `${SECURE_PREFIX}${key}`;
  if (Capacitor.isNativePlatform()) {
    try {
      await SecureStoragePlugin.remove({ key: fullKey });
    } catch {
      // ignore
    }
    return;
  }
  memoryStore.delete(fullKey);
}

export async function secureGetJson<T>(key: string): Promise<T | null> {
  const raw = await secureGet(key);
  if (!raw) return null;
  try {
    return JSON.parse(raw) as T;
  } catch {
    return null;
  }
}

export async function secureSetJson(key: string, value: unknown): Promise<void> {
  await secureSet(key, JSON.stringify(value));
}
