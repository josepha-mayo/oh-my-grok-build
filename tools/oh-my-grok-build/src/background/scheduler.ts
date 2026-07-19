import { CronJob } from "cron";
import { readFile, rm } from "node:fs/promises";
import { createInterface } from "node:readline";
import { createServer, createConnection, type Server } from "node:net";
import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { ensureOmgDir, getOmgDir, atomicWriteFile } from "../config.js";
import { runPromptTask } from "./runner.js";
import { appendTimelineEvent } from "../timeline.js";
import { withFileLock } from "../lock.js";

export interface JobMeta {
  name: string;
  expression: string;
  status: "active" | "stopped";
  meta?: Record<string, unknown>;
  createdAt: string;
  lastRun?: string;
}

interface ActiveJob {
  job: CronJob;
  expression: string;
}

const activeJobs = new Map<string, ActiveJob>();
const RECONCILE_MS = 2000;
const SOCKET_TIMEOUT_MS = 500;
const DAEMON_START_TIMEOUT_MS = 5000;

function schedulerPath(): string {
  return join(getOmgDir(), "scheduler.json");
}

async function withJobsLock<T>(fn: () => Promise<T>): Promise<T> {
  await ensureOmgDir();
  return withFileLock(schedulerPath(), fn);
}

export function controlSocketPath(): string {
  if (process.platform === "win32") {
    const hash = createHash("sha256").update(getOmgDir()).digest("hex").slice(0, 16);
    return `\\\\.\\pipe\\omgb-scheduler-${hash}`;
  }
  return join(getOmgDir(), "scheduler.sock");
}

export function entryScriptPath(): string {
  const current = fileURLToPath(import.meta.url);
  const ext = current.endsWith(".ts") ? ".ts" : ".js";
  return join(dirname(current), "..", `index${ext}`);
}

export function validateName(name: string): void {
  if (!/^[a-zA-Z0-9_-]{1,64}$/.test(name)) {
    throw new Error(`Invalid job name: ${name}`);
  }
}

export async function loadJobs(): Promise<JobMeta[]> {
  await ensureOmgDir();
  try {
    const raw = await readFile(schedulerPath(), "utf8");
    return JSON.parse(raw) as JobMeta[];
  } catch {
    return [];
  }
}

export async function saveJobs(jobs: JobMeta[]): Promise<void> {
  await ensureOmgDir();
  await atomicWriteFile(schedulerPath(), JSON.stringify(jobs, null, 2));
}

async function updateLastRun(name: string): Promise<void> {
  validateName(name);
  await withJobsLock(async () => {
    const jobs = await loadJobs();
    const job = jobs.find((j) => j.name === name);
    if (job) {
      job.lastRun = new Date().toISOString();
      await saveJobs(jobs);
    }
  });
}

function makeTask(name: string, meta?: Record<string, unknown>): () => Promise<void> {
  return async () => {
    const m = meta as { prompt?: string; model?: string; yolo?: boolean; cwd?: string } | undefined;
    if (!m?.prompt) {
      console.error(`[scheduler] job "${name}" has no prompt`);
      return;
    }
    await appendTimelineEvent({ type: "cron_run", name, model: m.model });
    try {
      await runPromptTask(m.prompt, { jobName: name, model: m.model, yolo: m.yolo, cwd: m.cwd });
    } catch (err) {
      console.error(`[scheduler] job "${name}" failed:`, err instanceof Error ? err.message : String(err));
    }
  };
}

function _startCronJob(name: string, expression: string, taskFn: () => Promise<void>): CronJob {
  const job = CronJob.from({
    cronTime: expression,
    onTick: async () => {
      await updateLastRun(name);
      try {
        await taskFn();
      } catch (err) {
        console.error(`[scheduler] job "${name}" failed:`, err instanceof Error ? err.message : String(err));
      }
    },
    start: true,
    waitForCompletion: true,
  });
  activeJobs.set(name, { job, expression });
  return job;
}

function _stopCronJob(name: string): void {
  const entry = activeJobs.get(name);
  if (entry) {
    entry.job.stop();
    activeJobs.delete(name);
  }
}

export async function startJob(
  name: string,
  cronExpression: string,
  taskFn: () => Promise<void>,
  meta?: Record<string, unknown>
): Promise<CronJob> {
  validateName(name);
  _stopCronJob(name);

  await withJobsLock(async () => {
    const jobs = await loadJobs();
    const now = new Date().toISOString();
    const entry: JobMeta = {
      name,
      expression: cronExpression,
      status: "active",
      meta,
      createdAt: now,
    };

    const idx = jobs.findIndex((j) => j.name === name);
    if (idx >= 0) {
      entry.createdAt = jobs[idx].createdAt;
      entry.lastRun = jobs[idx].lastRun;
      jobs[idx] = entry;
    } else {
      jobs.push(entry);
    }
    await saveJobs(jobs);
  });

  return _startCronJob(name, cronExpression, taskFn);
}

export async function saveCronJob(name: string, cronExpression: string, meta?: Record<string, unknown>): Promise<void> {
  validateName(name);
  await withJobsLock(async () => {
    const jobs = await loadJobs();
    const now = new Date().toISOString();
    const entry: JobMeta = {
      name,
      expression: cronExpression,
      status: "active",
      meta,
      createdAt: now,
    };

    const idx = jobs.findIndex((j) => j.name === name);
    if (idx >= 0) {
      entry.createdAt = jobs[idx].createdAt;
      entry.lastRun = jobs[idx].lastRun;
      jobs[idx] = entry;
    } else {
      jobs.push(entry);
    }
    await saveJobs(jobs);
  });
}

export async function stopJob(name: string): Promise<void> {
  validateName(name);
  _stopCronJob(name);

  await withJobsLock(async () => {
    const jobs = await loadJobs();
    const entry = jobs.find((j) => j.name === name);
    if (entry) {
      entry.status = "stopped";
      await saveJobs(jobs);
    }
  });
}

export async function deleteJob(name: string): Promise<void> {
  validateName(name);
  _stopCronJob(name);

  await withJobsLock(async () => {
    const jobs = await loadJobs();
    await saveJobs(jobs.filter((j) => j.name !== name));
  });
}

export async function listJobs(): Promise<JobMeta[]> {
  const jobs = await loadJobs();
  if (await isDaemonRunning()) {
    return jobs;
  }
  for (const job of jobs) {
    if (activeJobs.has(job.name)) job.status = "active";
    else if (job.status === "active") job.status = "stopped";
  }
  return jobs;
}

export async function runJobNow(name: string): Promise<boolean> {
  validateName(name);
  const entry = activeJobs.get(name);
  if (!entry) return false;
  await entry.job.fireOnTick();
  return true;
}

function socketCommand(cmd: "ping" | "stop"): Promise<{ ok?: boolean; pid?: number } | undefined> {
  const socketPath = controlSocketPath();
  return new Promise((resolve) => {
    const socket = createConnection(socketPath);
    const finish = (value: { ok?: boolean; pid?: number } | undefined) => {
      clearTimeout(timer);
      try {
        socket.end();
        socket.destroy();
      } catch {
        // ignore
      }
      resolve(value);
    };

    const timer = setTimeout(() => {
      finish(undefined);
    }, SOCKET_TIMEOUT_MS);

    socket.on("connect", () => {
      socket.write(JSON.stringify({ cmd }) + "\n");
    });
    socket.on("error", () => finish(undefined));
    socket.on("close", () => finish(undefined));

    const rl = createInterface({ input: socket, terminal: false });
    rl.on("line", (line) => {
      rl.close();
      try {
        finish(JSON.parse(line) as { ok?: boolean; pid?: number });
      } catch {
        finish(undefined);
      }
    });
    rl.on("error", () => finish(undefined));
  });
}

export async function isDaemonRunning(): Promise<boolean> {
  const result = await socketCommand("ping");
  return result?.ok === true;
}

export async function startSchedulerDaemon(): Promise<boolean> {
  if (await isDaemonRunning()) return false;

  const script = entryScriptPath();
  const proc = spawn(process.execPath, [script, "schedule", "daemon"], {
    detached: true,
    stdio: "ignore",
    env: process.env,
  });
  proc.unref();

  const deadline = Date.now() + DAEMON_START_TIMEOUT_MS;
  while (Date.now() < deadline) {
    await new Promise<void>((resolve) => setTimeout(resolve, 200));
    if (await isDaemonRunning()) return true;
    if (proc.exitCode !== null) return false;
  }
  return false;
}

export async function stopSchedulerDaemon(): Promise<boolean> {
  const running = await isDaemonRunning();
  if (!running) {
    await rm(controlSocketPath()).catch(() => {});
    return false;
  }
  const result = await socketCommand("stop");
  return result?.ok === true;
}

async function reconcile(): Promise<void> {
  const jobs = await withJobsLock(async () => {
    return loadJobs();
  });
  const desired = new Map(jobs.filter((j) => j.status === "active").map((j) => [j.name, j.expression]));

  for (const [name, { expression }] of Array.from(activeJobs.entries())) {
    if (!desired.has(name) || desired.get(name) !== expression) {
      _stopCronJob(name);
    }
  }

  for (const [name, expression] of desired.entries()) {
    if (!activeJobs.has(name)) {
      const meta = jobs.find((j) => j.name === name)?.meta;
      _startCronJob(name, expression, makeTask(name, meta));
    }
  }
}

function startControlServer(): Promise<Server> {
  const socketPath = controlSocketPath();
  return new Promise((resolve, reject) => {
    const server = createServer((socket) => {
      let buffer = "";
      socket.on("data", (chunk) => {
        buffer += chunk.toString("utf8");
        let idx;
        while ((idx = buffer.indexOf("\n")) !== -1) {
          const line = buffer.slice(0, idx).trim();
          buffer = buffer.slice(idx + 1);
          if (!line) continue;
          try {
            const msg = JSON.parse(line) as { cmd?: string };
            if (msg.cmd === "stop") {
              socket.write(JSON.stringify({ ok: true }) + "\n");
              socket.end();
              scheduleShutdown(server);
              return;
            }
            if (msg.cmd === "ping") {
              socket.write(JSON.stringify({ ok: true, pid: process.pid }) + "\n");
            }
          } catch {
            // ignore malformed commands
          }
        }
      });
    });

    server.on("error", (err) => {
      const code = (err as NodeJS.ErrnoException).code;
      if (code === "EADDRINUSE") {
        rm(socketPath)
          .then(() => server.listen(socketPath))
          .catch(() => reject(err));
      } else {
        reject(err);
      }
    });

    server.on("listening", () => resolve(server));
    server.listen(socketPath);
  });
}

let timer: ReturnType<typeof setTimeout> | undefined;

function scheduleShutdown(server: Server): void {
  clearTimeout(timer);
  for (const [name] of activeJobs) {
    _stopCronJob(name);
  }
  server.close(() => {
    rm(controlSocketPath())
      .catch(() => {})
      .finally(() => process.exit(0));
  });
  // Force exit if graceful close stalls.
  setTimeout(() => process.exit(0), 1000).unref?.();
}

export async function runSchedulerDaemon(): Promise<void> {
  await ensureOmgDir();
  const server = await startControlServer();

  let running = true;

  async function tick(): Promise<void> {
    if (!running) return;
    try {
      await reconcile();
    } catch (err) {
      console.error("[scheduler daemon] reconcile failed:", err instanceof Error ? err.message : String(err));
    }
    if (running) {
      timer = setTimeout(tick, RECONCILE_MS);
    }
  }

  process.on("SIGINT", () => {
    running = false;
    scheduleShutdown(server);
  });
  process.on("SIGTERM", () => {
    running = false;
    scheduleShutdown(server);
  });

  await tick();
  // Keep the daemon process alive until a signal shuts it down.
  await new Promise<void>(() => {});
}
