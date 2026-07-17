import { Buffer } from "node:buffer";
import chalk from "chalk";
import { loadOmgConfig } from "../config.js";
import spawner from "../spawner.js";
import { listSubagents, spawnSubagent, subagentOutput } from "../subagents/engine.js";

export interface SwarmOptions {
  prompt: string;
  workers?: number;
  timeout?: number;
  model?: string;
  yolo?: boolean;
  maxTurns?: number;
  cwd?: string;
}

const DEFAULT_TIMEOUT = 10 * 60 * 1000;

function clampWorkers(n: number | undefined): number {
  const value = n ?? 4;
  return Math.max(1, Math.min(20, value));
}

function runGrokCapture(
  prompt: string,
  options: { model: string; yolo?: boolean; cwd?: string }
): Promise<{ code: number | null; output: string }> {
  const args = ["-p", prompt, "--model", options.model];
  if (options.yolo) args.push("--yolo");
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    const proc = spawner.spawn("grok", args, {
      cwd: options.cwd ?? process.cwd(),
      env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
      stdio: ["ignore", "pipe", "pipe"],
    });
    proc.stdout?.on("data", (d) => chunks.push(d));
    proc.stderr?.on("data", (d) => chunks.push(d));
    proc.on("error", reject);
    proc.on("exit", (code) => {
      resolve({ code, output: Buffer.concat(chunks).toString("utf8") });
    });
  });
}

async function decomposeTask(
  prompt: string,
  workers: number,
  options: { model: string; yolo?: boolean; cwd?: string }
): Promise<string[]> {
  const task = `Decompose the following task into at most ${workers} concise subtasks. Reply with a single JSON array of strings and nothing else.\n\nTask: ${prompt}`;
  const { code, output } = await runGrokCapture(task, options);
  if (code !== 0) throw new Error(`grok decompose exited with code ${code}`);

  try {
    const parsed = JSON.parse(output.trim());
    if (Array.isArray(parsed) && parsed.every((x) => typeof x === "string")) {
      return parsed.slice(0, workers);
    }
  } catch {
    // fallthrough to line-based fallback
  }

  const lines = output
    .split("\n")
    .map((l) => l.replace(/^\s*[-*\d.]+\s*/, "").trim())
    .filter((l) => l.length > 0);
  return lines.length > 0 ? lines.slice(0, workers) : [prompt];
}

async function waitForSubagents(names: string[], timeoutMs: number): Promise<void> {
  const start = Date.now();
  while (true) {
    const all = await listSubagents();
    const ours = all.filter((a) => names.includes(a.name));
    if (ours.every((a) => !a.running)) return;
    if (Date.now() - start > timeoutMs) throw new Error("Swarm timed out waiting for subagents");
    await new Promise((r) => setTimeout(r, 500));
  }
}

export async function swarmCommand(options: SwarmOptions): Promise<void> {
  const workers = clampWorkers(options.workers);
  const timeoutMs = options.timeout ?? DEFAULT_TIMEOUT;
  const cfg = await loadOmgConfig();
  const model = options.model ?? cfg.defaultModel ?? "grok-build";
  const spawnOptions = { model, yolo: options.yolo, maxTurns: options.maxTurns, cwd: options.cwd };

  console.log(chalk.bold(`Decomposing task into up to ${workers} subtasks with model ${chalk.cyan(model)}...`));
  const subtasks = await decomposeTask(options.prompt, workers, { model, yolo: options.yolo, cwd: options.cwd });

  console.log(chalk.bold(`Spawning ${subtasks.length} subagent(s)...`));
  const names: string[] = [];
  for (let i = 0; i < subtasks.length; i++) {
    const name = `swarm-${i}`;
    await spawnSubagent(name, subtasks[i], spawnOptions);
    names.push(name);
  }

  console.log(chalk.dim(`Waiting up to ${timeoutMs / 1000}s for subagents...`));
  await waitForSubagents(names, timeoutMs);

  console.log(chalk.bold("\nAggregated results:\n"));
  for (const name of names) {
    const output = await subagentOutput(name, 50);
    console.log(chalk.cyan(`--- ${name} ---`));
    console.log(output.trim() || chalk.dim("(no output)"));
    console.log("");
  }
}
