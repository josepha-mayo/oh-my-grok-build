import { appendFileSync, mkdirSync } from "node:fs";
import { join, resolve, normalize, sep } from "node:path";
import { getOmgDir, loadOmgConfig } from "../config.js";
import spawner from "../spawner.js";

export interface RunPromptOptions {
  jobName?: string;
  model?: string;
  yolo?: boolean;
  maxTurns?: number;
  cwd?: string;
}

function sanitizeJobName(name?: string): string {
  const safe = (name ?? "default")
    .toString()
    .replace(/[^a-zA-Z0-9_-]/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
  return safe || "default";
}

function logPath(jobName?: string): string {
  const logsDir = resolve(join(getOmgDir(), "logs"));
  mkdirSync(logsDir, { recursive: true });
  const safeName = sanitizeJobName(jobName);
  const candidate = resolve(join(logsDir, `${safeName}.jsonl`));
  const normalized = normalize(candidate);
  const normalizedLogs = normalize(logsDir);
  if (!normalized.toLowerCase().startsWith(normalizedLogs.toLowerCase() + sep) && normalized !== normalizedLogs) {
    throw new Error(`Invalid job name: ${jobName ?? "default"}`);
  }
  return candidate;
}

function appendLog(file: string, stream: "stdout" | "stderr", data: Buffer): void {
  const line = JSON.stringify({
    ts: new Date().toISOString(),
    stream,
    line: data.toString("utf8"),
  });
  appendFileSync(file, `${line}\n`);
}

export async function runPromptTask(prompt: string, options: RunPromptOptions = {}): Promise<void> {
  const cfg = await loadOmgConfig();
  const model = options.model ?? cfg.defaultModel ?? "grok-build";

  const args = ["-p", prompt, "--model", model];
  if (options.yolo) args.push("--yolo");
  if (options.maxTurns) args.push("--max-turns", String(options.maxTurns));

  const file = logPath(options.jobName);

  return new Promise((resolve, reject) => {
    const proc = spawner.spawn("grok", args, {
      cwd: options.cwd ?? process.cwd(),
      env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
    });

    proc.stdout?.on("data", (d) => appendLog(file, "stdout", d));
    proc.stderr?.on("data", (d) => appendLog(file, "stderr", d));
    proc.on("error", reject);
    proc.on("exit", (code) => {
      if (code === 0) resolve();
      else reject(new Error(`grok exited with code ${code}`));
    });
  });
}
