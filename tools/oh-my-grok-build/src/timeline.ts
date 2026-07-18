import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { getOmgDir } from "./config.js";

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

export function appendTimelineEvent(event: Omit<TimelineEvent, "ts">): void {
  ensureTimelineDir();
  const path = timelinePath();
  const line = JSON.stringify({ ts: new Date().toISOString(), ...event });
  appendFileSync(path, `${line}\n`);

  if (existsSync(path)) {
    try {
      const raw = readFileSync(path, "utf8");
      const lines = raw.split("\n").filter(Boolean);
      if (lines.length > MAX_EVENTS) {
        writeFileSync(path, lines.slice(lines.length - MAX_EVENTS).join("\n") + "\n");
      }
    } catch {
      // ignore truncation failures
    }
  }
}

export function readTimelineEvents(options?: { count?: number; type?: string }): TimelineEvent[] {
  const path = timelinePath();
  if (!existsSync(path)) return [];
  try {
    const lines = readFileSync(path, "utf8")
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
    const count = options?.count ?? 50;
    return filtered.slice(-count);
  } catch {
    return [];
  }
}
