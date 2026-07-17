import { homedir } from "os";
import { mkdirSync, existsSync, readFileSync, writeFileSync } from "fs";
import { join } from "path";

const GROK_HOME = process.env.GROK_HOME ?? join(homedir(), ".grok");

export const OMGB_HOME = process.env.OMGB_HOME ?? join(homedir(), ".omgb");
export const OMGB_TASTE_DIR = join(OMGB_HOME, "taste");
export const OMGB_STATE = join(OMGB_HOME, "state.json");

export function ensureHome(): void {
  mkdirSync(OMGB_HOME, { recursive: true });
  mkdirSync(OMGB_TASTE_DIR, { recursive: true });
}

export function loadState<T = unknown>(): T {
  ensureHome();
  if (!existsSync(OMGB_STATE)) return {} as T;
  return JSON.parse(readFileSync(OMGB_STATE, "utf8")) as T;
}

export function saveState(state: unknown): void {
  ensureHome();
  writeFileSync(OMGB_STATE, JSON.stringify(state, null, 2));
}

export { GROK_HOME };
