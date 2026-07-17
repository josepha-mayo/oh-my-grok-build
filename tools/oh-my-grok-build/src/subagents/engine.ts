import { spawn, spawnSync } from "node:child_process";
import { openSync, readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { ensureOmgDir, getOmgDir } from "../config.js";

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
}

function registryPath(): string {
  return join(getOmgDir(), "subagents.json");
}

function subagentsDir(): string {
  return join(getOmgDir(), "subagents");
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
  writeFileSync(registryPath(), JSON.stringify(records, null, 2));
}

export function isRunning(pid: number): boolean {
  try {
    return process.kill(pid, 0);
  } catch {
    return false;
  }
}

function setupWorktree(worktree: string): void {
  mkdirSync(subagentsDir(), { recursive: true });

  const gitCheck = spawnSync("git", ["rev-parse", "--is-inside-work-tree"], {
    cwd: process.cwd(),
    encoding: "utf8",
  });

  if (gitCheck.status === 0 && gitCheck.stdout?.trim() === "true") {
    const add = spawnSync("git", ["worktree", "add", "--detach", worktree], {
      cwd: process.cwd(),
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
  const worktree = join(subagentsDir(), name);
  const logPath = join(worktree, "subagent.log");

  setupWorktree(worktree);

  const records = await loadRegistry();
  const existing = records.findIndex((r) => r.name === name);
  if (existing >= 0) records.splice(existing, 1);

  const logFd = openSync(logPath, "a");

  const args = ["-p", prompt, "--model", options.model ?? "grok-build"];
  if (options.yolo) args.push("--yolo");
  if (options.maxTurns) args.push("--max-turns", String(options.maxTurns));

  const proc = spawn("grok", args, {
    cwd: worktree,
    env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
    detached: true,
    stdio: ["ignore", logFd, logFd],
  });

  proc.unref();

  const record: SubagentRecord = {
    name,
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

  return { ...record, running: true };
}

export async function listSubagents(): Promise<SubagentStatus[]> {
  const records = await loadRegistry();
  return records.map((r) => ({ ...r, running: isRunning(r.pid) }));
}

export async function killSubagent(name: string): Promise<void> {
  const records = await loadRegistry();
  const idx = records.findIndex((r) => r.name === name);
  if (idx === -1) throw new Error(`Subagent "${name}" not found`);

  const record = records[idx];
  try {
    process.kill(record.pid, "SIGTERM");
  } catch (err) {
    throw new Error(`Failed to kill subagent "${name}": ${err instanceof Error ? err.message : String(err)}`);
  }

  records.splice(idx, 1);
  await saveRegistry(records);
}

export async function subagentOutput(name: string, tailLines = 50): Promise<string> {
  const records = await loadRegistry();
  const record = records.find((r) => r.name === name);
  if (!record) throw new Error(`Subagent "${name}" not found`);

  try {
    const lines = readFileSync(record.logPath, "utf8").split("\n");
    return lines.slice(-tailLines).join("\n");
  } catch {
    return "";
  }
}
