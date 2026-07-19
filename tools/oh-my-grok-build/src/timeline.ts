import { appendFile, readFile, writeFile } from "node:fs/promises";
import { existsSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { getOmgDir } from "./config.js";
import { withFileLock } from "./lock.js";

const MAX_EVENTS = 5000;

export interface TimelineEvent {
  ts: string;
  type: string;
  [key: string]: unknown;
}

function timelinePath(): string {
  return join(getOmgDir(), "timeline.jsonl");
}

function ensureTimelineDir(): void {
  mkdirSync(getOmgDir(), { recursive: true });
}

export async function appendTimelineEvent(event: Omit<TimelineEvent, "ts">): Promise<void> {
  ensureTimelineDir();
  const path = timelinePath();
  try {
    await withFileLock(path, async () => {
      const line = JSON.stringify({ ts: new Date().toISOString(), ...event });
      await appendFile(path, `${line}\n`);

      try {
        const raw = await readFile(path, "utf8");
        const lines = raw.split("\n").filter(Boolean);
        if (lines.length > MAX_EVENTS) {
          await writeFile(path, lines.slice(lines.length - MAX_EVENTS).join("\n") + "\n");
        }
      } catch {
        // ignore truncation failures
      }
    });
  } catch {
    // Timeline writes are best-effort; do not crash the caller.
  }
}

export async function readTimelineEvents(options?: { count?: number; type?: string }): Promise<TimelineEvent[]> {
  const path = timelinePath();
  if (!existsSync(path)) return [];
  try {
    return await withFileLock(path, async () => {
      try {
        const lines = (await readFile(path, "utf8"))
          .split("\n")
          .filter(Boolean)
          .map((line) => {
            try {
              return JSON.parse(line) as TimelineEvent;
            } catch {
              return undefined;
            }
          })
          .filter((e): e is TimelineEvent => e !== undefined);

        const filtered = options?.type ? lines.filter((e) => e.type === options.type) : lines;
        const count = Number.isNaN(options?.count) ? 50 : (options?.count ?? 50);
        return filtered.slice(-count);
      } catch {
        return [];
      }
    });
  } catch {
    return [];
  }
}
