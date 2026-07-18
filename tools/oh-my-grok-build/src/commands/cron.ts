import chalk from "chalk";
import { runPromptTask } from "../background/runner.js";
import { startJob, stopJob } from "../background/scheduler.js";
import { appendTimelineEvent } from "../timeline.js";

export interface CronOptions {
  expression: string;
  prompt: string;
  model?: string;
  yolo?: boolean;
}

export async function cronCommand(options: CronOptions): Promise<void> {
  const name = "cron";

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

  console.log(chalk.bold(`Cron started:`), chalk.cyan(options.expression));
  console.log(chalk.dim(`Press Ctrl+C to stop.`));

  process.on("SIGINT", async () => {
    appendTimelineEvent({ type: "cron_stop", expression: options.expression });
    await stopJob(name);
    process.exit(0);
  });
}
