import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import chalk from "chalk";
import { DEFAULT_MODEL, loadOmgConfig } from "../config.js";
import spawner from "../spawner.js";
import { isRateLimited, formatRateLimitMessage } from "../rate-limit.js";
import { appendTimelineEvent } from "../timeline.js";
import { withTaste } from "../taste.js";

export interface TeamOptions {
  count: number;
  model?: string;
  prompt: string;
  yolo?: boolean;
}

export async function teamCommand(options: TeamOptions): Promise<void> {
  const cfg = await loadOmgConfig();
  const model = options.model ?? cfg.defaultModel ?? DEFAULT_MODEL;
  const prompt = await withTaste(options.prompt);

  let count = Number.isNaN(options.count) ? 1 : options.count;
  count = Math.max(1, Math.min(20, count));

  await appendTimelineEvent({ type: "team_start", model, count, prompt });

  console.log(chalk.bold(`Spawning ${count} Grok worker(s) with model ${chalk.cyan(model)}...\n`));

  const workers = Array.from({ length: count }, async (_, i) => {
    const workdir = await mkdtemp(join(tmpdir(), `omgb-team-${i}-`));
    const args = ["-p", prompt, "--model", model];
    if (options.yolo) args.push("--yolo");

    return new Promise<{ index: number; output: string; code: number | null }>((resolve) => {
      const chunks: Buffer[] = [];
      const proc = spawner.spawn("grok", args, {
        cwd: workdir,
      });
      proc.stdout?.on("data", (d) => chunks.push(d));
      proc.stderr?.on("data", (d) => chunks.push(d));
      proc.on("exit", (code) => {
        resolve({ index: i, output: Buffer.concat(chunks).toString("utf8"), code });
      });
    });
  });

  const results = await Promise.all(workers);
  await appendTimelineEvent({
    type: "team_stop",
    count,
    results: results.map((r) => ({ index: r.index, exitCode: r.code })),
  });
  for (const r of results) {
    console.log(chalk.cyan(`\n--- Worker ${r.index + 1} (exit ${r.code ?? "?"}) ---`));
    if (r.code !== 0 && r.code !== null && isRateLimited(r.output)) {
      console.log(chalk.yellow(formatRateLimitMessage()));
    } else {
      console.log(r.output.trim() || chalk.dim("(no output)"));
    }
  }
}
