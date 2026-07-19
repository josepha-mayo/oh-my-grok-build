import chalk from "chalk";
import { addTasteNote, loadTasteNotes, removeTasteNote } from "../taste.js";
import { appendTimelineEvent } from "../timeline.js";

export async function tasteListCommand(): Promise<void> {
  const notes = await loadTasteNotes();
  if (notes.likes.length === 0 && notes.dislikes.length === 0) {
    console.log(chalk.dim("No taste notes yet. Use `omgb taste like <note>` or `omgb taste dislike <note>`."));
    return;
  }
  if (notes.likes.length) {
    console.log(chalk.bold("\nLikes:"));
    notes.likes.forEach((n, i) => console.log(`  ${i + 1}. ${n}`));
  }
  if (notes.dislikes.length) {
    console.log(chalk.bold("\nDislikes:"));
    notes.dislikes.forEach((n, i) => console.log(`  ${i + 1}. ${n}`));
  }
}

export async function tasteAddCommand(kind: "like" | "dislike", note: string): Promise<void> {
  if (!note.trim()) throw new Error("Taste note cannot be empty");
  await addTasteNote(kind, note.trim());
  await appendTimelineEvent({ type: "taste_add", kind, note: note.trim() });
  console.log(chalk.green(`Added ${kind}: ${note.trim()}`));
}

export async function tasteRemoveCommand(kind: "like" | "dislike", index: number): Promise<void> {
  if (!Number.isInteger(index) || index < 1) {
    throw new Error(`Taste note number must be a positive integer, got ${index}`);
  }
  const humanIndex = index - 1;
  await removeTasteNote(kind, humanIndex);
  await appendTimelineEvent({ type: "taste_remove", kind, index: humanIndex });
  console.log(chalk.green(`Removed ${kind} #${index}.`));
}
