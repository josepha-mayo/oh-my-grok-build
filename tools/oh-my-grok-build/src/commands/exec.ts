import { loadOmgConfig } from "../config.js";
import spawner from "../spawner.js";

export interface ExecOptions {
  prompt: string;
  model?: string;
  yolo?: boolean;
  cwd?: string;
  maxTurns?: number;
}

export async function execCommand(options: ExecOptions): Promise<void> {
  const cfg = await loadOmgConfig();
  const model = options.model ?? cfg.defaultModel ?? "grok-build";

  const args = ["-p", options.prompt, "--model", model];
  if (options.yolo) args.push("--yolo");
  if (options.maxTurns) args.push("--max-turns", String(options.maxTurns));

  return new Promise((resolve, reject) => {
    const proc = spawner.spawn("grok", args, {
      cwd: options.cwd ?? process.cwd(),
      stdio: "inherit",
      env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
    });
    proc.on("error", reject);
    proc.on("exit", (code) => {
      if (code === 0) resolve();
      else reject(new Error(`grok exited with code ${code}`));
    });
  });
}
