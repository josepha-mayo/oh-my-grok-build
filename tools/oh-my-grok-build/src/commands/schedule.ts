import chalk from "chalk";
import { deleteJob, listJobs, loadJobs, runJobNow, stopJob } from "../background/scheduler.js";
import { runPromptTask } from "../background/runner.js";

export async function scheduleListCommand(): Promise<void> {
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
  await stopJob(name);
  console.log(chalk.bold(`Stopped job:`), chalk.cyan(name));
}

export async function scheduleRunCommand(name: string): Promise<void> {
  const ran = await runJobNow(name);
  if (ran) {
    console.log(chalk.bold(`Ran job:`), chalk.cyan(name));
    return;
  }

  const jobs = await loadJobs();
  const job = jobs.find((j) => j.name === name);
  const meta = job?.meta as { prompt?: string; model?: string; yolo?: boolean } | undefined;
  if (meta?.prompt) {
    await runPromptTask(meta.prompt, { jobName: name, model: meta.model, yolo: meta.yolo });
    console.log(chalk.bold(`Ran job from stored task:`), chalk.cyan(name));
    return;
  }

  console.error(chalk.red(`No active or stored job named "${name}".`));
  process.exitCode = 1;
}

export async function scheduleDeleteCommand(name: string): Promise<void> {
  await deleteJob(name);
  console.log(chalk.bold(`Deleted job:`), chalk.cyan(name));
}
