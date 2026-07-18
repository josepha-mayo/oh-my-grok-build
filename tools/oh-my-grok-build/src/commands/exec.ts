import { loadOmgConfig } from "../config.js";
import spawner from "../spawner.js";
import { isRateLimited, formatRateLimitMessage } from "../rate-limit.js";
import { appendTimelineEvent } from "../timeline.js";

export interface ExecOptions {
  prompt: string;
  model?: string;
  yolo?: boolean;
  cwd?: string;
  maxTurns?: number;
}

export async function execCommand(options: ExecOptions): Promise<void> {
  const cfg = await loadOmgConfig();
  const model = options.model ?? cfg.defaultModel ?? "grok-4.5";

  appendTimelineEvent({ type: "exec_start", model, prompt: options.prompt, cwd: options.cwd ?? process.cwd() });

  const args = ["-p", options.prompt, "--model", model];
  if (options.yolo) args.push("--yolo");
  if (typeof options.maxTurns === "number" && !Number.isNaN(options.maxTurns) && options.maxTurns > 0) {
    args.push("--max-turns", String(options.maxTurns));
  }

  return new Promise((resolve, reject) => {
    const stderrChunks: Buffer[] = [];
    const proc = spawner.spawn("grok", args, {
      cwd: options.cwd ?? process.cwd(),
      stdio: ["inherit", "inherit", "pipe"],
      env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
    });
    proc.stderr?.on("data", (d: Buffer) => {
      stderrChunks.push(d);
      process.stderr.write(d);
    });
    proc.on("error", reject);
    proc.on("exit", (code) => {
      appendTimelineEvent({ type: code === 0 ? "exec_stop" : "exec_error", model, exitCode: code });
      if (code === 0) {
        resolve();
        return;
      }
      const stderr = Buffer.concat(stderrChunks).toString("utf8");
      if (isRateLimited(stderr)) {
        reject(new Error(formatRateLimitMessage()));
      } else {
        reject(new Error(`grok exited with code ${code}`));
      }
    });
  });
}
