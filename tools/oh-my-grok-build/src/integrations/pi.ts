import type { Connector, ConnectorConfig, ConnectorResult } from "./types.js";
import { sanitizeUserEnv } from "../env.js";
import spawner from "../spawner.js";

export class PiConnector implements Connector {
  constructor(readonly config: ConnectorConfig) {}

  async run(prompt: string): Promise<ConnectorResult> {
    const cmd = this.config.command ?? "pi";
    const args = ["-p", prompt];

    return new Promise<ConnectorResult>((resolve, reject) => {
      const chunks: Buffer[] = [];
      const proc = spawner.spawn(cmd, args, {
        cwd: this.config.cwd ?? process.cwd(),
        env: { ...process.env, ...sanitizeUserEnv(this.config.env) },
      });

      proc.stdout?.on("data", (d) => chunks.push(d));
      proc.stderr?.on("data", (d) => chunks.push(d));
      proc.on("error", reject);
      proc.on("exit", (code) => {
        const text = Buffer.concat(chunks).toString("utf8");
        if (code !== 0 && code !== null) {
          const snippet = text.slice(-500);
          reject(new Error(`${cmd} exited with code ${code}${snippet ? `: ${snippet}` : ""}`));
          return;
        }
        resolve({ text, usage: { exitCode: code ?? 0 } });
      });
    });
  }
}
