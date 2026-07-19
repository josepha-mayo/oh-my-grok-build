import chalk from "chalk";
import { DEFAULT_MODEL, loadGrokConfig, loadOmgConfig } from "../config.js";
import spawner from "../spawner.js";
import { appendTimelineEvent } from "../timeline.js";
import { withTaste } from "../taste.js";

export interface DevinAutonomousOptions {
  prompt: string;
  model?: string;
  sandboxProfile?: string;
  cwd?: string;
}

export async function devinAutonomousCommand(options: DevinAutonomousOptions): Promise<void> {
  const cwd = options.cwd ?? process.cwd();
  const prompt = await withTaste(options.prompt);
  await appendTimelineEvent({
    type: "autonomous_start",
    model: options.model,
    prompt,
    cwd,
    sandboxProfile: options.sandboxProfile,
  });

  const cfg = await loadGrokConfig();
  const configProfile = (cfg.sandbox as Record<string, unknown> | undefined)?.profile as string | undefined;
  const sandboxProfile = options.sandboxProfile ?? configProfile;

  if (!sandboxProfile || sandboxProfile === "off") {
    console.warn(
      chalk.yellow(
        "Warning: Devin autonomous mode should run inside a sandbox. Set [sandbox].profile in ~/.grok/config.toml or use --sandbox-profile."
      )
    );
  }

  const ocfg = await loadOmgConfig();
  const model = options.model ?? ocfg.defaultModel ?? DEFAULT_MODEL;
  const args = ["-p", prompt, "--yolo", "--model", model];

  const env: NodeJS.ProcessEnv = { ...process.env };
  if (sandboxProfile && sandboxProfile !== "off") {
    env.GROK_SANDBOX_PROFILE = sandboxProfile;
  }

  console.log(chalk.bold(`Running devin autonomous with model ${chalk.cyan(model)}...`));

  return new Promise((resolve, reject) => {
    const proc = spawner.spawn("grok", args, {
      cwd,
      stdio: "inherit",
      env,
    });
    proc.on("error", reject);
    proc.on("exit", async (code) => {
      await appendTimelineEvent({ type: code === 0 ? "autonomous_stop" : "autonomous_error", exitCode: code });
      if (code === 0) resolve();
      else reject(new Error(`grok exited with code ${code}`));
    });
  });
}
