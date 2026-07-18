import chalk from "chalk";
import { runPromptTask } from "../background/runner.js";
import {
  loadJobs,
  saveCronJob,
  startJob,
  startSchedulerDaemon,
  stopJob,
  validateName,
} from "../background/scheduler.js";
import { appendTimelineEvent } from "../timeline.js";

export interface CronOptions {
  expression: string;
  prompt: string;
  name?: string;
  model?: string;
  yolo?: boolean;
  cwd?: string;
  daemon?: boolean;
}

async function uniqueCronName(base = "cron"): Promise<string> {
  const jobs = await loadJobs();
  const names = new Set(jobs.map((j) => j.name));
  if (!names.has(base)) return base;
  let i = 1;
  while (names.has(`${base}-${i}`)) i++;
  return `${base}-${i}`;
}

export async function cronCommand(options: CronOptions): Promise<void> {
  const name = options.name ?? (await uniqueCronName());
  validateName(name);

  appendTimelineEvent({
    type: "cron_start",
    expression: options.expression,
    prompt: options.prompt,
    model: options.model,
  });

  const meta = { prompt: options.prompt, model: options.model, yolo: options.yolo, cwd: options.cwd ?? process.cwd() };

  if (options.daemon) {
    await saveCronJob(name, options.expression, meta);
    const started = await startSchedulerDaemon();
    console.log(
      chalk.bold(`Scheduled cron job:`),
      chalk.cyan(name),
      chalk.dim(options.expression),
      started ? chalk.dim("(scheduler daemon started)") : ""
    );
    return;
  }

  await startJob(
    name,
    options.expression,
    async () => {
      await runPromptTask(options.prompt, {
        jobName: name,
        model: options.model,
        yolo: options.yolo,
        cwd: options.cwd ?? process.cwd(),
      });
    },
    meta
  );

  console.log(chalk.bold(`Cron started:`), chalk.cyan(name), chalk.dim(options.expression));
  console.log(chalk.dim(`Press Ctrl+C to stop.`));

  const onShutdown = () => {
    appendTimelineEvent({ type: "cron_stop", expression: options.expression, name });
    void stopJob(name)
      .catch(() => undefined)
      .finally(() => process.exit(0));
  };
  process.on("SIGINT", onShutdown);
  process.on("SIGTERM", onShutdown);
}
