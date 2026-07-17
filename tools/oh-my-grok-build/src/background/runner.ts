import { spawn } from "node:child_process";
import { appendFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";
import { getOmgDir, loadOmgConfig } from "../config.js";

export interface RunPromptOptions {
  jobName?: string;
  model?: string;
  yolo?: boolean;
  maxTurns?: number;
  cwd?: string;
}

function logPath(jobName?: string): string {
  const logsDir = join(getOmgDir(), "logs");
  mkdirSync(logsDir, { recursive: true });
  return join(logsDir, `${jobName ?? "default"}.jsonl`);
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
    const proc = spawn("grok", args, {
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
