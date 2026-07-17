import { spawn } from "node:child_process";
import type { Connector, ConnectorConfig, ConnectorResult } from "./types.js";

export class ClaudeConnector implements Connector {
  constructor(readonly config: ConnectorConfig) {}

  async run(prompt: string): Promise<ConnectorResult> {
    const cmd = this.config.command ?? "claude";
    const args = ["--bare", "-p", prompt, "--allowedTools", "Bash,Read,Edit,View", "--output-format", "text"];
    if (this.config.cwd) args.push("--cwd", this.config.cwd);

    return new Promise((resolve, reject) => {
      const chunks: Buffer[] = [];
      const proc = spawn(cmd, args, {
        cwd: this.config.cwd ?? process.cwd(),
        env: { ...process.env, ...this.config.env },
      });

      proc.stdout?.on("data", (d) => chunks.push(d));
      proc.stderr?.on("data", (d) => chunks.push(d));
      proc.on("error", reject);
      proc.on("exit", (code) => {
        const text = Buffer.concat(chunks).toString("utf8");
        resolve({ text, usage: code !== null ? { exitCode: code } : undefined });
      });
    });
  }
}
