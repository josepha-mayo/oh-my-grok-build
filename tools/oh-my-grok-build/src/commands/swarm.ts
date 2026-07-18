import { Buffer } from "node:buffer";
import chalk from "chalk";
import { loadOmgConfig } from "../config.js";
import spawner from "../spawner.js";
import { isRateLimited, formatRateLimitMessage } from "../rate-limit.js";
import { listSubagents, spawnSubagent, subagentOutput, killSubagent } from "../subagents/engine.js";
import { appendTimelineEvent } from "../timeline.js";

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
  const value = Number.isNaN(n) ? 4 : (n ?? 4);
  return Math.max(1, Math.min(20, value));
}

const MAX_CAPTURE_BYTES = 10 * 1024 * 1024;

function runGrokCapture(
  prompt: string,
  options: { model: string; yolo?: boolean; cwd?: string }
): Promise<{ code: number | null; output: string }> {
  const args = ["-p", prompt, "--model", options.model];
  if (options.yolo) args.push("--yolo");
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    let killed = false;
    let byteCount = 0;
    const proc = spawner.spawn("grok", args, {
      cwd: options.cwd ?? process.cwd(),
      env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
      stdio: ["ignore", "pipe", "pipe"],
    });
    proc.stdout?.on("data", (d: Buffer) => {
      if (killed) return;
      byteCount += d.length;
      if (byteCount > MAX_CAPTURE_BYTES) {
        killed = true;
        proc.kill("SIGTERM");
        chunks.push(Buffer.from("\n[captured output truncated]"));
        return;
      }
      chunks.push(d);
    });
    proc.stderr?.on("data", (d: Buffer) => {
      if (!killed) chunks.push(d);
    });
    proc.on("error", reject);
    proc.on("exit", (code) => {
      resolve({ code, output: Buffer.concat(chunks).toString("utf8") });
    });
  });
}

function extractJsonArray(output: string): unknown {
  const trimmed = output.trim();
  const fenced = trimmed.match(/```(?:json)?\s*([\s\S]*?)```/);
  const candidate = fenced ? fenced[1].trim() : trimmed;
  try {
    return JSON.parse(candidate);
  } catch {
    return undefined;
  }
}

function validateSubtasks(parsed: unknown, workers: number): string[] {
  if (!Array.isArray(parsed)) return [];
  const items = parsed
    .map((x) =>
      typeof x === "string"
        ? x.trim()
        : typeof x === "object" && x && "task" in x && typeof (x as { task: unknown }).task === "string"
          ? (x as { task: string }).task.trim()
          : ""
    )
    .filter((x) => x.length > 0);
  return items.slice(0, workers);
}

async function decomposeTask(
  prompt: string,
  workers: number,
  options: { model: string; yolo?: boolean; cwd?: string }
): Promise<string[]> {
  const makePrompt = (extra = "") =>
    [
      `Decompose the following task into at most ${workers} concise subtasks.`,
      `Reply with a single JSON array of strings. Example: ["subtask one", "subtask two"].`,
      extra,
      `\nTask: ${prompt}`,
    ].join("\n");

  for (let attempt = 1; attempt <= 2; attempt++) {
    const task = makePrompt(
      attempt > 1 ? "\nYour previous response was not valid JSON. Please reply with only the JSON array." : ""
    );
    const { code, output } = await runGrokCapture(task, options);
    if (code !== 0) {
      if (isRateLimited(output)) throw new Error(formatRateLimitMessage());
      throw new Error(`grok decompose exited with code ${code}`);
    }

    const parsed = extractJsonArray(output);
    const valid = validateSubtasks(parsed, workers);
    if (valid.length > 0) return valid;
  }

  // Final fallback: split the prompt into sentences and treat each as a subtask.
  const fallback = prompt
    .split(/[.!?]\n+|[.!?]\s+/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0)
    .slice(0, workers);
  return fallback.length > 0 ? fallback : [prompt];
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
  const timeoutMs = Number.isNaN(options.timeout) ? DEFAULT_TIMEOUT : (options.timeout ?? DEFAULT_TIMEOUT);
  const cfg = await loadOmgConfig();
  const model = options.model ?? cfg.defaultModel ?? "grok-build";
  const spawnOptions = { model, yolo: options.yolo, maxTurns: options.maxTurns, cwd: options.cwd };

  // Use a unique run prefix so repeated swarms do not collide with old worktrees.
  const runId = Date.now();
  appendTimelineEvent({ type: "swarm_start", model, workers, prompt: options.prompt, runId });

  const names: string[] = [];
  for (let i = 0; i < workers; i++) {
    names.push(`swarm-${runId}-${i}`);
  }

  // Clean up any previous swarm worktrees with the same index names to avoid
  // stale processes and git worktree conflicts.
  for (const name of names) {
    try {
      await killSubagent(name);
    } catch {
      // not running; ignore
    }
  }

  console.log(chalk.bold(`Decomposing task into up to ${workers} subtasks with model ${chalk.cyan(model)}...`));
  const subtasks = await decomposeTask(options.prompt, workers, { model, yolo: options.yolo, cwd: options.cwd });

  console.log(chalk.bold(`Spawning ${subtasks.length} subagent(s)...`));
  for (let i = 0; i < subtasks.length; i++) {
    await spawnSubagent(names[i], subtasks[i], spawnOptions);
  }

  console.log(chalk.dim(`Waiting up to ${timeoutMs / 1000}s for subagents...`));
  await waitForSubagents(names, timeoutMs);

  console.log(chalk.bold("\nAggregated results:\n"));
  const outputs: { name: string; output: string }[] = [];
  for (const name of names) {
    const output = await subagentOutput(name, 50);
    console.log(chalk.cyan(`--- ${name} ---`));
    if (isRateLimited(output)) {
      console.log(chalk.yellow(formatRateLimitMessage()));
    } else {
      console.log(output.trim() || chalk.dim("(no output)"));
    }
    console.log("");
    outputs.push({ name, output });
  }

  appendTimelineEvent({
    type: "swarm_stop",
    runId,
    workers: names.length,
    outputs: outputs.map((o) => ({ name: o.name, preview: o.output.slice(0, 200) })),
  });
}
