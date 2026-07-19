import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { addTasteNote, loadTasteNotes, removeTasteNote, formatTasteContext, withTaste } from "./taste.js";
import { setupOmgHome, cleanupOmgHome } from "./test-utils.js";

let tempDir: string;

describe("taste", () => {
  beforeEach(() => {
    tempDir = setupOmgHome();
  });

  afterEach(() => {
    cleanupOmgHome(tempDir);
  });

  it("starts empty", async () => {
    const notes = await loadTasteNotes();
    assert.deepEqual(notes, { likes: [], dislikes: [] });
    assert.equal(await withTaste("hello"), "hello");
  });

  it("adds and lists likes and dislikes", async () => {
    await addTasteNote("like", "Use async/await");
    await addTasteNote("dislike", "Avoid var");
    const notes = await loadTasteNotes();
    assert.deepEqual(notes.likes, ["Use async/await"]);
    assert.deepEqual(notes.dislikes, ["Avoid var"]);
  });

  it("removes notes by index", async () => {
    await addTasteNote("like", "A");
    await addTasteNote("like", "B");
    await removeTasteNote("like", 0);
    const notes = await loadTasteNotes();
    assert.deepEqual(notes.likes, ["B"]);
    await assert.rejects(removeTasteNote("like", 5));
    await assert.rejects(removeTasteNote("like", 1.5));
    await assert.rejects(removeTasteNote("like", -1));
    await assert.rejects(removeTasteNote("neutral" as any, 0));
  });

  it("rejects invalid taste notes", async () => {
    await assert.rejects(addTasteNote("neutral" as any, "note"));
    await assert.rejects(addTasteNote("like", 123 as any));
    await addTasteNote("like", "   ");
    const notes = await loadTasteNotes();
    assert.deepEqual(notes.likes, []);
  });

  it("formats taste context", () => {
    const ctx = formatTasteContext({ likes: ["Like"], dislikes: ["Dislike"] });
    assert.ok(ctx.includes("Like"));
    assert.ok(ctx.includes("Dislike"));
  });

  it("injects taste into a prompt", async () => {
    await addTasteNote("like", "Use semicolons");
    const prompt = await withTaste("Write code");
    assert.ok(prompt.includes("Use semicolons"));
    assert.ok(prompt.endsWith("Write code"));
  });

  it("creates the omgb home directory if it does not exist", async () => {
    const originalHome = process.env.OMGB_HOME;
    const missingDir = mkdtempSync(join(tmpdir(), "omgb-missing-"));
    rmSync(missingDir, { recursive: true, force: true });
    process.env.OMGB_HOME = missingDir;
    try {
      await addTasteNote("like", "Works without existing dir");
      const notes = await loadTasteNotes();
      assert.deepEqual(notes.likes, ["Works without existing dir"]);
    } finally {
      cleanupOmgHome(missingDir);
      process.env.OMGB_HOME = originalHome;
    }
  });
});
