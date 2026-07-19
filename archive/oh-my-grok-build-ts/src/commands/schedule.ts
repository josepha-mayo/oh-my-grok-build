import chalk from "chalk";
import {
  deleteJob,
  isDaemonRunning,
  listJobs,
  loadJobs,
  runJobNow,
  runSchedulerDaemon,
  startSchedulerDaemon,
  stopJob,
  stopSchedulerDaemon,
  validateName,
} from "../background/scheduler.js";
import { runPromptTask } from "../background/runner.js";

export async function scheduleListCommand(): Promise<void> {
  const daemon = await isDaemonRunning();
  console.log(chalk.dim(`Scheduler daemon: ${daemon ? chalk.green("running") : chalk.yellow("not running")}`));
  const jobs = await listJobs();
  if (jobs.length === 0) {
    console.log(chalk.dim("No scheduled jobs."));
    return;
  }
  for (const job of jobs) {
    console.log(
      `${chalk.cyan(job.name)} [${job.status}] ${chalk.dim(job.expression)}` +
        (job.lastRun ? ` last-run ${job.lastRun}` : "")
    );
  }
}

export async function scheduleStopCommand(name: string): Promise<void> {
  validateName(name);
  await stopJob(name);
  console.log(chalk.bold(`Stopped job:`), chalk.cyan(name));
}

export async function scheduleRunCommand(name: string): Promise<void> {
  validateName(name);
  const ran = await runJobNow(name);
  if (ran) {
    console.log(chalk.bold(`Ran job:`), chalk.cyan(name));
    return;
  }

  const jobs = await loadJobs();
  const job = jobs.find((j) => j.name === name);
  const meta = job?.meta as { prompt?: string; model?: string; yolo?: boolean; cwd?: string } | undefined;
  if (meta?.prompt) {
    await runPromptTask(meta.prompt, { jobName: name, model: meta.model, yolo: meta.yolo, cwd: meta.cwd });
    console.log(chalk.bold(`Ran job from stored task:`), chalk.cyan(name));
    return;
  }

  console.error(chalk.red(`No active or stored job named "${name}".`));
  process.exitCode = 1;
}

export async function scheduleDeleteCommand(name: string): Promise<void> {
  validateName(name);
  await deleteJob(name);
  console.log(chalk.bold(`Deleted job:`), chalk.cyan(name));
}

export async function scheduleStartCommand(): Promise<void> {
  if (await isDaemonRunning()) {
    console.log(chalk.yellow("Scheduler daemon is already running."));
    return;
  }
  const started = await startSchedulerDaemon();
  if (started) {
    console.log(chalk.green("Scheduler daemon started."));
  } else {
    console.error(chalk.red("Could not start scheduler daemon."));
    process.exitCode = 1;
  }
}

export async function scheduleStopDaemonCommand(): Promise<void> {
  const stopped = await stopSchedulerDaemon();
  console.log(stopped ? chalk.green("Scheduler daemon stopped.") : chalk.yellow("Scheduler daemon was not running."));
}

export async function scheduleStatusCommand(): Promise<void> {
  await scheduleListCommand();
}

export async function scheduleDaemonCommand(): Promise<void> {
  await runSchedulerDaemon();
}
