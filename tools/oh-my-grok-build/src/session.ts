import { readdirSync, existsSync, readFileSync } from "fs";
import { join } from "path";
import { GROK_HOME } from "./config.js";

const SESSIONS_DIR = join(GROK_HOME, "sessions");

export interface SessionMeta {
  sessionId: string;
  cwd: string;
  title?: string;
  updatedAt?: string;
}

export function listSessions(limit = 20): SessionMeta[] {
  if (!existsSync(SESSIONS_DIR)) return [];
  const sessions: SessionMeta[] = [];
  for (const group of readdirSync(SESSIONS_DIR, { withFileTypes: true })) {
    if (!group.isDirectory()) continue;
    const groupDir = join(SESSIONS_DIR, group.name);
    for (const entry of readdirSync(groupDir, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const summaryFile = join(groupDir, entry.name, "summary.json");
      if (!existsSync(summaryFile)) continue;
      const summary = JSON.parse(readFileSync(summaryFile, "utf8")) as Record<string, unknown>;
      sessions.push({
        sessionId: entry.name,
        cwd: group.name,
        title: (summary.generated_title as string) ?? (summary.session_summary as string) ?? undefined,
        updatedAt: summary.updated_at as string,
      });
    }
  }
  return sessions
    .sort((a, b) => (b.updatedAt ?? "").localeCompare(a.updatedAt ?? ""))
    .slice(0, limit);
}
