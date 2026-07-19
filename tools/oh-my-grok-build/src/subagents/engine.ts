import { openSync, readFileSync, writeFileSync, mkdirSync, appendFileSync } from "node:fs";
import { join } from "node:path";
import { atomicWriteFile, ensureOmgDir, getOmgDir, DEFAULT_MODEL } from "../config.js";
import spawner from "../spawner.js";
import { appendTimelineEvent } from "../timeline.js";

export interface SubagentRecord {
  name: string;
  pid: number;
  worktree: string;
  logPath: string;
  prompt: string;
  model?: string;
  yolo?: boolean;
  maxTurns?: number;
  spawnedAt: string;
}

export interface SubagentStatus extends SubagentRecord {
  running: boolean;
}

export interface SpawnSubagentOptions {
  model?: string;
  yolo?: boolean;
  maxTurns?: number;
  cwd?: string;
}

function registryPath(): string {
  return join(getOmgDir(), "subagents.json");
}

function subagentsDir(): string {
  return join(getOmgDir(), "subagents");
}

function tracePath(worktree: string): string {
  return join(worktree, "trace.jsonl");
}

export async function loadRegistry(): Promise<SubagentRecord[]> {
  await ensureOmgDir();
  try {
    return JSON.parse(readFileSync(registryPath(), "utf8")) as SubagentRecord[];
  } catch {
    return [];
  }
}

export async function saveRegistry(records: SubagentRecord[]): Promise<void> {
  await ensureOmgDir();
  await atomicWriteFile(registryPath(), JSON.stringify(records, null, 2));
}

export function isRunning(pid: number): boolean {
  try {
    return process.kill(pid, 0);
  } catch {
    return false;
  }
}

function sanitizeSubagentName(name: string): string {
  const sanitized = name
    .replace(/[^a-zA-Z0-9_-]/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
  if (!sanitized) throw new Error("Invalid subagent name");
  return sanitized;
}

function setupWorktree(worktree: string, cwd: string): void {
  mkdirSync(subagentsDir(), { recursive: true });

  const gitCheck = spawner.spawnSync("git", ["rev-parse", "--is-inside-work-tree"], {
    cwd,
    encoding: "utf8",
  });

  if (gitCheck.status === 0 && gitCheck.stdout?.toString().trim() === "true") {
    const add = spawner.spawnSync("git", ["worktree", "add", "--detach", worktree], {
      cwd,
      encoding: "utf8",
    });
    if (add.status === 0) return;
  }

  mkdirSync(worktree, { recursive: true });
}

export async function spawnSubagent(
  name: string,
  prompt: string,
  options: SpawnSubagentOptions = {}
): Promise<SubagentStatus> {
  const safeName = sanitizeSubagentName(name);
  const worktree = join(subagentsDir(), safeName);
  const logPath = join(worktree, "subagent.log");
  const traceFile = tracePath(worktree);
  const repoRoot = options.cwd ?? process.cwd();

  setupWorktree(worktree, repoRoot);

  const records = await loadRegistry();
  const existing = records.findIndex((r) => r.name === safeName);
  if (existing >= 0) records.splice(existing, 1);

  appendFileSync(
    traceFile,
    JSON.stringify({
      ts: new Date().toISOString(),
      event: "spawn",
      name: safeName,
      prompt,
      model: options.model,
      yolo: options.yolo,
      maxTurns: options.maxTurns,
    }) + "\n"
  );

  const logFd = openSync(logPath, "a");

  const args = ["-p", prompt, "--model", options.model ?? DEFAULT_MODEL];
  if (options.yolo) args.push("--yolo");
  if (options.maxTurns) args.push("--max-turns", String(options.maxTurns));

  const proc = spawner.spawn("grok", args, {
    cwd: worktree,
    env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
    detached: true,
    stdio: ["ignore", logFd, logFd],
  });

  proc.unref();

  const record: SubagentRecord = {
    name: safeName,
    pid: proc.pid ?? 0,
    worktree,
    logPath,
    prompt,
    model: options.model,
    yolo: options.yolo,
    maxTurns: options.maxTurns,
    spawnedAt: new Date().toISOString(),
  };

  records.push(record);
  await saveRegistry(records);

  await appendTimelineEvent({
    type: "subagent_spawn",
    name: safeName,
    model: options.model,
    prompt,
    pid: proc.pid ?? 0,
  });

  return { ...record, running: true };
}

export async function listSubagents(): Promise<SubagentStatus[]> {
  const records = await loadRegistry();
  return records.map((r) => ({ ...r, running: isRunning(r.pid) }));
}

export async function killSubagent(name: string): Promise<void> {
  const safeName = sanitizeSubagentName(name);
  const records = await loadRegistry();
  const idx = records.findIndex((r) => r.name === safeName);
  if (idx === -1) throw new Error(`Subagent "${safeName}" not found`);

  const record = records[idx];
  try {
    if (isRunning(record.pid)) {
      // On Unix, kill the whole process group to avoid leaving orphans.
      if (process.platform !== "win32") {
        try {
          process.kill(-record.pid, "SIGTERM");
        } catch {
          process.kill(record.pid, "SIGTERM");
        }
      } else {
        process.kill(record.pid, "SIGTERM");
      }
    }
  } catch (err) {
    throw new Error(`Failed to kill subagent "${safeName}": ${err instanceof Error ? err.message : String(err)}`);
  }

  await appendTimelineEvent({ type: "subagent_kill", name: safeName, pid: record.pid });

  records.splice(idx, 1);
  await saveRegistry(records);
}

export async function subagentOutput(name: string, rawTailLines = 50): Promise<string> {
  const tailLines = Number.isNaN(rawTailLines) ? 50 : rawTailLines;
  const safeName = sanitizeSubagentName(name);
  const records = await loadRegistry();
  const record = records.find((r) => r.name === safeName);
  if (!record) throw new Error(`Subagent "${safeName}" not found`);

  try {
    const lines = readFileSync(record.logPath, "utf8").split("\n");
    return lines.slice(-tailLines).join("\n");
  } catch {
    return "";
  }
}

export async function subagentTrace(name: string, rawTailLines = 50): Promise<string> {
  const tailLines = Number.isNaN(rawTailLines) ? 50 : rawTailLines;
  const safeName = sanitizeSubagentName(name);
  const records = await loadRegistry();
  const record = records.find((r) => r.name === safeName);
  if (!record) throw new Error(`Subagent "${safeName}" not found`);

  const parts: string[] = [];
  parts.push(`Subagent: ${record.name}`);
  parts.push(`Model: ${record.model ?? DEFAULT_MODEL}`);
  parts.push(`Spawned: ${record.spawnedAt}`);
  parts.push(`Worktree: ${record.worktree}`);
  parts.push(`Prompt: ${record.prompt}`);
  parts.push("");

  try {
    const traceRaw = readFileSync(tracePath(record.worktree), "utf8").trim();
    if (traceRaw) {
      const traceLines = traceRaw.split("\n").filter(Boolean);
      parts.push("--- trace ---");
      for (const line of traceLines.slice(-tailLines)) {
        try {
          const entry = JSON.parse(line) as { ts?: string; event?: string; prompt?: string };
          const ts = entry.ts ? new Date(entry.ts).toLocaleTimeString() : "";
          if (entry.event === "spawn" && entry.prompt) {
            parts.push(`${ts} [spawn] prompt: ${entry.prompt.slice(0, 200)}`);
          } else if (entry.event && entry.event !== "spawn") {
            parts.push(`${ts} [${entry.event}]`);
          }
        } catch {
          parts.push(line);
        }
      }
      parts.push("");
    }
  } catch {
    // trace file may not exist yet
  }

  try {
    const output = readFileSync(record.logPath, "utf8").trim();
    if (output) {
      parts.push("--- output ---");
      parts.push(output.split("\n").slice(-tailLines).join("\n"));
    }
  } catch {
    // no output yet
  }

  return parts.join("\n");
}
