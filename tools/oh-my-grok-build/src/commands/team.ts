import { spawn } from "node:child_process";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import chalk from "chalk";
import { loadOmgConfig } from "../config.js";

export interface TeamOptions {
  count: number;
  model?: string;
  prompt: string;
  yolo?: boolean;
}

export async function teamCommand(options: TeamOptions): Promise<void> {
  const cfg = await loadOmgConfig();
  const model = options.model ?? cfg.defaultModel ?? "grok-build";

  console.log(chalk.bold(`Spawning ${options.count} Grok worker(s) with model ${chalk.cyan(model)}...\n`));

  const workers = Array.from({ length: options.count }, async (_, i) => {
    const workdir = await mkdtemp(join(tmpdir(), `omgb-team-${i}-`));
    const args = ["-p", options.prompt, "--model", model];
    if (options.yolo) args.push("--yolo");

    return new Promise<{ index: number; output: string; code: number | null }>((resolve) => {
      const chunks: Buffer[] = [];
      const proc = spawn("grok", args, {
        cwd: workdir,
        env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
      });
      proc.stdout?.on("data", (d) => chunks.push(d));
      proc.stderr?.on("data", (d) => chunks.push(d));
      proc.on("exit", (code) => {
        resolve({ index: i, output: Buffer.concat(chunks).toString("utf8"), code });
      });
    });
  });

  const results = await Promise.all(workers);
  for (const r of results) {
    console.log(chalk.cyan(`\n--- Worker ${r.index + 1} (exit ${r.code ?? "?"}) ---`));
    console.log(r.output.trim() || chalk.dim("(no output)"));
  }
}
