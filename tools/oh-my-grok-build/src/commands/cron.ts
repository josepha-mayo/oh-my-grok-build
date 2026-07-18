import chalk from "chalk";
import { runPromptTask } from "../background/runner.js";
import { loadJobs, startJob, stopJob } from "../background/scheduler.js";
import { appendTimelineEvent } from "../timeline.js";

export interface CronOptions {
  expression: string;
  prompt: string;
  name?: string;
  model?: string;
  yolo?: boolean;
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

  appendTimelineEvent({
    type: "cron_start",
    expression: options.expression,
    prompt: options.prompt,
    model: options.model,
  });

  await startJob(
    name,
    options.expression,
    async () => {
      await runPromptTask(options.prompt, {
        jobName: name,
        model: options.model,
        yolo: options.yolo,
      });
    },
    { prompt: options.prompt, model: options.model, yolo: options.yolo }
  );

  console.log(chalk.bold(`Cron started:`), chalk.cyan(name), chalk.dim(options.expression));
  console.log(chalk.dim(`Press Ctrl+C to stop.`));

  process.on("SIGINT", () => {
    appendTimelineEvent({ type: "cron_stop", expression: options.expression, name });
    void stopJob(name)
      .catch(() => undefined)
      .finally(() => process.exit(0));
  });
}
