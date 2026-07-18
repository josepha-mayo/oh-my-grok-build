import { CronJob } from "cron";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { ensureOmgDir, getOmgDir, atomicWriteFile } from "../config.js";

export interface JobMeta {
  name: string;
  expression: string;
  status: "active" | "stopped";
  meta?: Record<string, unknown>;
  createdAt: string;
  lastRun?: string;
}

const activeJobs = new Map<string, CronJob>();

function schedulerPath(): string {
  return join(getOmgDir(), "scheduler.json");
}

function validateName(name: string): void {
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
  const jobs = await loadJobs();
  const job = jobs.find((j) => j.name === name);
  if (job) {
    job.lastRun = new Date().toISOString();
    await saveJobs(jobs);
  }
}

export async function startJob(
  name: string,
  cronExpression: string,
  taskFn: () => Promise<void>,
  meta?: Record<string, unknown>
): Promise<CronJob> {
  validateName(name);
  if (activeJobs.has(name)) {
    const old = activeJobs.get(name)!;
    old.stop();
    activeJobs.delete(name);
  }

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
  if (idx >= 0) jobs[idx] = entry;
  else jobs.push(entry);
  await saveJobs(jobs);

  const job = CronJob.from({
    cronTime: cronExpression,
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

  activeJobs.set(name, job);
  return job;
}

export async function stopJob(name: string): Promise<void> {
  validateName(name);
  const job = activeJobs.get(name);
  if (job) {
    job.stop();
    activeJobs.delete(name);
  }

  const jobs = await loadJobs();
  const entry = jobs.find((j) => j.name === name);
  if (entry) {
    entry.status = "stopped";
    await saveJobs(jobs);
  }
}

export async function deleteJob(name: string): Promise<void> {
  validateName(name);
  await stopJob(name);
  const jobs = await loadJobs();
  await saveJobs(jobs.filter((j) => j.name !== name));
}

export async function listJobs(): Promise<JobMeta[]> {
  const jobs = await loadJobs();
  for (const job of jobs) {
    if (activeJobs.has(job.name)) job.status = "active";
    else if (job.status === "active") job.status = "stopped";
  }
  return jobs;
}

export async function runJobNow(name: string): Promise<boolean> {
  validateName(name);
  const job = activeJobs.get(name);
  if (!job) return false;
  await job.fireOnTick();
  return true;
}
