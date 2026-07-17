import chalk from "chalk";
import { runPromptTask } from "../background/runner.js";
import { startJob, stopJob } from "../background/scheduler.js";

export interface LoopOptions {
  expression: string;
  prompt: string;
  model?: string;
  yolo?: boolean;
}

export async function loopCommand(options: LoopOptions): Promise<void> {
  const name = "loop";

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

  console.log(chalk.bold(`Loop started:`), chalk.cyan(options.expression));
  console.log(chalk.dim(`Press Ctrl+C to stop.`));

  process.on("SIGINT", async () => {
    await stopJob(name);
    process.exit(0);
  });
}
