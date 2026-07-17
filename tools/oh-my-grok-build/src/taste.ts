import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync, rmSync } from "fs";
import { join } from "path";
import { OMGB_TASTE_DIR, ensureHome } from "./config.js";
import type { TastePackage } from "./types.js";

function pkgPath(name: string): string {
  return join(OMGB_TASTE_DIR, `${name}.json`);
}

export function listTaste(): TastePackage[] {
  ensureHome();
  if (!existsSync(OMGB_TASTE_DIR)) return [];
  return readdirSync(OMGB_TASTE_DIR)
    .filter((f) => f.endsWith(".json"))
    .map((f) => JSON.parse(readFileSync(join(OMGB_TASTE_DIR, f), "utf8")) as TastePackage);
}

export function getTaste(name: string): TastePackage | undefined {
  const p = pkgPath(name);
  if (!existsSync(p)) return undefined;
  return JSON.parse(readFileSync(p, "utf8")) as TastePackage;
}

export function setTaste(pkg: TastePackage): void {
  ensureHome();
  writeFileSync(pkgPath(pkg.name), JSON.stringify(pkg, null, 2));
}

export function removeTaste(name: string): void {
  const p = pkgPath(name);
  if (existsSync(p)) rmSync(p);
}

export function learnFromSession(sessionDir: string): TastePackage[] {
  // Placeholder: derive taste from Grok session log. A real impl would parse updates.jsonl.
  ensureHome();
  const detected: TastePackage = {
    name: "auto-learned",
    category: "general",
    confidence: 0.5,
    learned: [`Analyzed session dir ${sessionDir}`],
  };
  setTaste(detected);
  return [detected];
}
