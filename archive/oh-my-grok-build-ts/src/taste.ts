import { readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { ensureOmgDir, getOmgDir } from "./config.js";

export interface TasteNotes {
  /** Preferences the user has explicitly stated (do this). */
  likes: string[];
  /** Things the user has explicitly asked to avoid (do not do this). */
  dislikes: string[];
}

function tastePath(): string {
  return join(getOmgDir(), "taste.json");
}

export async function loadTasteNotes(): Promise<TasteNotes> {
  const path = tastePath();
  if (!existsSync(path)) return { likes: [], dislikes: [] };
  try {
    const raw = await readFile(path, "utf8");
    const parsed = JSON.parse(raw) as Partial<TasteNotes>;
    return {
      likes: Array.isArray(parsed.likes) ? parsed.likes.filter((n) => typeof n === "string") : [],
      dislikes: Array.isArray(parsed.dislikes) ? parsed.dislikes.filter((n) => typeof n === "string") : [],
    };
  } catch {
    return { likes: [], dislikes: [] };
  }
}

export async function saveTasteNotes(notes: TasteNotes): Promise<void> {
  await ensureOmgDir();
  await writeFile(tastePath(), JSON.stringify(notes, null, 2), { mode: 0o600 });
}

export async function addTasteNote(kind: "like" | "dislike", note: string): Promise<void> {
  if (kind !== "like" && kind !== "dislike") {
    throw new Error(`Invalid taste kind: ${kind}`);
  }
  if (typeof note !== "string") {
    throw new Error("Taste note must be a string");
  }
  const trimmed = note.trim();
  if (!trimmed) return;
  const notes = await loadTasteNotes();
  const target = kind === "like" ? notes.likes : notes.dislikes;
  if (target.includes(trimmed)) return;
  target.push(trimmed);
  await saveTasteNotes(notes);
}

export async function removeTasteNote(kind: "like" | "dislike", index: number): Promise<void> {
  if (kind !== "like" && kind !== "dislike") {
    throw new Error(`Invalid taste kind: ${kind}`);
  }
  if (!Number.isInteger(index) || index < 0) {
    throw new Error(`Invalid taste ${kind} index: ${index}`);
  }
  const notes = await loadTasteNotes();
  const target = kind === "like" ? notes.likes : notes.dislikes;
  if (index >= target.length) throw new Error(`Invalid taste ${kind} index: ${index}`);
  target.splice(index, 1);
  await saveTasteNotes(notes);
}

export function formatTasteContext(notes: TasteNotes): string {
  const parts: string[] = [];
  if (notes.likes.length) {
    parts.push("Preferences to keep in mind:\n- " + notes.likes.join("\n- "));
  }
  if (notes.dislikes.length) {
    parts.push("Things to avoid:\n- " + notes.dislikes.join("\n- "));
  }
  if (!parts.length) return "";
  return ["[Taste]", ...parts, ""].join("\n");
}

export async function tasteContext(): Promise<string> {
  return formatTasteContext(await loadTasteNotes());
}

export async function withTaste(prompt: string): Promise<string> {
  const context = await tasteContext();
  if (!context) return prompt;
  return `${context}\n${prompt}`;
}
