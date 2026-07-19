import { appendFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import type { ChildProcess } from "node:child_process";
import chalk from "chalk";
import { DEFAULT_MODEL, getOmgDir, loadOmgConfig } from "../config.js";
import { isRateLimited, formatRateLimitMessage } from "../rate-limit.js";
import spawner from "../spawner.js";
import { appendTimelineEvent } from "../timeline.js";

export interface LoopOptions {
  prompt: string;
  model?: string;
  yolo?: boolean;
  maxIterations?: number;
  cwd?: string;
}

const DIFF_MAX_BYTES = 1_000_000;

function logsDir(): string {
  return join(getOmgDir(), "logs");
}

function logPath(): string {
  return join(logsDir(), "loop.jsonl");
}

function ensureLogsDir(): void {
  mkdirSync(logsDir(), { recursive: true });
}

function appendLoopLog(entry: Record<string, unknown>): void {
  ensureLogsDir();
  appendFileSync(logPath(), JSON.stringify({ ts: new Date().toISOString(), ...entry }) + "\n");
}

function gitOutput(cwd: string, args: string[], maxBytes = DIFF_MAX_BYTES): Promise<string> {
  return new Promise((resolve, reject) => {
    let output = "";
    let stderr = "";
    let killed = false;
    const proc = spawner.spawn("git", args, { cwd, env: process.env }) as ChildProcess;
    proc.stdout?.setEncoding("utf8");
    proc.stdout?.on("data", (chunk: string) => {
      if (killed) return;
      output += chunk;
      if (Buffer.byteLength(output, "utf8") > maxBytes) {
        killed = true;
        proc.kill("SIGTERM");
        output += "\n[truncated: diff exceeded size limit]";
      }
    });
    proc.stderr?.setEncoding("utf8");
    proc.stderr?.on("data", (chunk: string) => {
      stderr += chunk;
    });
    proc.on("error", reject);
    proc.on("exit", (code) => {
      if (code !== 0 && code !== null) {
        const message = output.trim() || stderr.trim() || "(no output)";
        const err = new Error(`git ${args.join(" ")} exited with code ${code}: ${message}`) as Error & {
          code?: number | null;
        };
        err.code = code;
        reject(err);
        return;
      }
      if (stderr && !output.trim()) {
        reject(new Error(`git ${args.join(" ")} failed: ${stderr.trim()}`));
        return;
      }
      resolve(output);
    });
  });
}

function gitStatusShort(cwd: string): Promise<string> {
  return gitOutput(cwd, ["status", "--short"], DIFF_MAX_BYTES);
}

function gitDiff(cwd: string): Promise<string> {
  return gitOutput(cwd, ["diff"], DIFF_MAX_BYTES);
}

function isNotGitRepo(err: unknown): boolean {
  if (err instanceof Error) {
    if ((err as Error & { code?: number | null }).code === 128) return true;
    if (err.message.includes("not a git repository")) return true;
  }
  return false;
}

interface GrokRunResult {
  code: number | null;
  stderr: string;
}

function runGrokOnce(prompt: string, options: { cwd: string; model: string; yolo?: boolean }): Promise<GrokRunResult> {
  const args = ["-p", prompt, "--model", options.model];
  if (options.yolo) args.push("--yolo");
  return new Promise((resolve, reject) => {
    let stderr = "";
    const proc = spawner.spawn("grok", args, {
      cwd: options.cwd,
      stdio: ["inherit", "inherit", "pipe"],
      env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
    });
    proc.stderr?.setEncoding("utf8");
    proc.stderr?.on("data", (chunk: string) => {
      stderr += chunk;
      process.stderr.write(chunk);
    });
    proc.on("error", reject);
    proc.on("exit", (code) => resolve({ code, stderr }));
  });
}

export async function loopCommand(options: LoopOptions): Promise<void> {
  const cwd = options.cwd ?? process.cwd();
  const cfg = await loadOmgConfig();
  const model = options.model ?? cfg.defaultModel ?? DEFAULT_MODEL;
  const rawMax = Number.isNaN(options.maxIterations) ? 5 : (options.maxIterations ?? 5);
  const maxIterations = Math.max(1, Math.min(50, rawMax));

  await appendTimelineEvent({ type: "loop_start", model, maxIterations, prompt: options.prompt, cwd });

  let inRepo = true;
  let initialStatus = "";
  try {
    initialStatus = await gitStatusShort(cwd);
  } catch (err) {
    if (isNotGitRepo(err)) {
      inRepo = false;
    } else {
      throw err;
    }
  }
  if (inRepo && initialStatus.trim()) {
    throw new Error("Working tree is not clean. Commit or stash changes before starting a loop.");
  }

  console.log(chalk.bold(`Starting loop with model ${chalk.cyan(model)} (max ${maxIterations} iterations)...`));

  let currentPrompt = options.prompt;
  let lastExit: number | null = null;
  let iteration = 0;

  while (iteration < maxIterations) {
    iteration++;
    console.log(chalk.dim(`\n--- Iteration ${iteration} ---`));

    const { code: exitCode, stderr } = await runGrokOnce(currentPrompt, { cwd, model, yolo: options.yolo });
    lastExit = exitCode;

    if (lastExit !== 0) {
      await appendTimelineEvent({ type: "loop_error", model, iterations: iteration, exitCode: lastExit });
      if (isRateLimited(stderr)) {
        throw new Error(formatRateLimitMessage());
      }
      throw new Error(`grok exited with code ${lastExit}`);
    }

    let diff = "";
    let status = "";
    if (inRepo) {
      diff = await gitDiff(cwd);
      status = await gitStatusShort(cwd);
    }

    appendLoopLog({
      iteration,
      prompt: currentPrompt,
      exitCode: lastExit,
      dirty: status.trim().length > 0,
      diffLength: diff.length,
      statusLength: status.length,
    });

    if (inRepo && !status.trim()) {
      console.log(chalk.green("Working tree is clean. Stopping."));
      break;
    }

    if (inRepo) {
      currentPrompt = `Original task: ${options.prompt}\n\nReview the following diff and fix any issues:\n\n${diff || status}`;
    } else {
      console.log(chalk.dim("Not a git repository; continuing with the original prompt."));
      currentPrompt = options.prompt;
    }
  }

  const finalDirty = inRepo ? (await gitStatusShort(cwd)).trim().length > 0 : false;
  await appendTimelineEvent({ type: "loop_stop", model, iterations: iteration, dirty: finalDirty });

  if (finalDirty) {
    console.warn(chalk.yellow("Warning: working tree is still dirty after max iterations."));
  }
}
