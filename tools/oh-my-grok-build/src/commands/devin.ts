import { appendFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import chalk from "chalk";
import { getOmgDir, loadGrokConfig, loadOmgConfig } from "../config.js";
import spawner from "../spawner.js";

export interface DevinLoopOptions {
  prompt: string;
  model?: string;
  yolo?: boolean;
  maxIterations?: number;
  cwd?: string;
}

export interface DevinAutonomousOptions {
  prompt: string;
  model?: string;
  sandboxProfile?: string;
  cwd?: string;
}

function logsDir(): string {
  return join(getOmgDir(), "logs");
}

function logPath(): string {
  return join(logsDir(), "devin-loop.jsonl");
}

function ensureLogsDir(): void {
  mkdirSync(logsDir(), { recursive: true });
}

function appendLoopLog(entry: Record<string, unknown>): void {
  ensureLogsDir();
  appendFileSync(logPath(), JSON.stringify({ ts: new Date().toISOString(), ...entry }) + "\n");
}

function gitStatusShort(cwd: string): string {
  const result = spawner.spawnSync("git", ["status", "--short"], { cwd, encoding: "utf8" });
  return result.stdout?.toString() ?? "";
}

function gitDiff(cwd: string): string {
  const result = spawner.spawnSync("git", ["diff"], { cwd, encoding: "utf8" });
  return result.stdout?.toString() ?? "";
}

function runGrokOnce(prompt: string, options: { cwd: string; model: string; yolo?: boolean }): Promise<number | null> {
  const args = ["-p", prompt, "--model", options.model];
  if (options.yolo) args.push("--yolo");
  return new Promise((resolve, reject) => {
    const proc = spawner.spawn("grok", args, {
      cwd: options.cwd,
      stdio: "inherit",
      env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
    });
    proc.on("error", reject);
    proc.on("exit", (code) => resolve(code));
  });
}

export async function devinLoopCommand(options: DevinLoopOptions): Promise<void> {
  const cwd = options.cwd ?? process.cwd();
  const cfg = await loadOmgConfig();
  const model = options.model ?? cfg.defaultModel ?? "grok-build";
  const maxIterations = options.maxIterations ?? 5;

  if (gitStatusShort(cwd).trim()) {
    throw new Error("Working tree is not clean. Commit or stash changes before starting a devin loop.");
  }

  console.log(chalk.bold(`Starting devin loop with model ${chalk.cyan(model)} (max ${maxIterations} iterations)...`));

  let currentPrompt = options.prompt;
  let lastExit: number | null = null;
  let iteration = 0;

  while (iteration < maxIterations) {
    iteration++;
    console.log(chalk.dim(`\n--- Iteration ${iteration} ---`));

    lastExit = await runGrokOnce(currentPrompt, { cwd, model, yolo: options.yolo });

    const diff = gitDiff(cwd);
    const status = gitStatusShort(cwd);

    appendLoopLog({
      iteration,
      prompt: currentPrompt,
      exitCode: lastExit,
      dirty: status.trim().length > 0,
      diffLength: diff.length,
      statusLength: status.length,
    });

    if (!status.trim()) {
      console.log(chalk.green("Working tree is clean. Stopping."));
      break;
    }

    currentPrompt = `Review the following diff and fix any issues:\n\n${diff || status}`;
  }

  if (lastExit !== 0) {
    throw new Error(`grok exited with code ${lastExit}`);
  }

  if (gitStatusShort(cwd).trim()) {
    console.warn(chalk.yellow("Warning: working tree is still dirty after max iterations."));
  }
}

export async function devinAutonomousCommand(options: DevinAutonomousOptions): Promise<void> {
  const cwd = options.cwd ?? process.cwd();
  const cfg = await loadGrokConfig();
  const configProfile = (cfg.sandbox as Record<string, unknown> | undefined)?.profile as string | undefined;

  if (!options.sandboxProfile && (!configProfile || configProfile === "off")) {
    console.warn(
      chalk.yellow(
        "Warning: Devin autonomous mode should run inside a sandbox. Set [sandbox].profile in ~/.grok/config.toml or use --sandbox-profile."
      )
    );
  }

  const ocfg = await loadOmgConfig();
  const model = options.model ?? ocfg.defaultModel ?? "grok-build";
  const args = ["-p", options.prompt, "--yolo", "--model", model];

  const env: NodeJS.ProcessEnv = { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" };
  if (options.sandboxProfile) {
    env.GROK_SANDBOX_PROFILE = options.sandboxProfile;
  }

  console.log(chalk.bold(`Running devin autonomous with model ${chalk.cyan(model)}...`));

  return new Promise((resolve, reject) => {
    const proc = spawner.spawn("grok", args, {
      cwd,
      stdio: "inherit",
      env,
    });
    proc.on("error", reject);
    proc.on("exit", (code) => {
      if (code === 0) resolve();
      else reject(new Error(`grok exited with code ${code}`));
    });
  });
}
