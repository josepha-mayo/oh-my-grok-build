import { spawn } from "node:child_process";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Connector, ConnectorConfig, ConnectorResult } from "./types.js";

export class CodexConnector implements Connector {
  constructor(readonly config: ConnectorConfig) {}

  async run(prompt: string): Promise<ConnectorResult> {
    const tmpDir = await mkdtemp(join(tmpdir(), "omgb-codex-"));
    const lastMessageFile = join(tmpDir, "last-message.txt");

    const args = [
      "exec",
      "--json",
      "--sandbox",
      "workspace-write",
      "--output-last-message",
      lastMessageFile,
      prompt,
    ];
    if (this.config.cwd) args.push("--cwd", this.config.cwd);

    return new Promise((resolve, reject) => {
      const chunks: Buffer[] = [];
      const proc = spawn("codex", args, {
        cwd: this.config.cwd ?? process.cwd(),
        env: { ...process.env, ...this.config.env },
      });

      proc.stdout?.on("data", (d) => chunks.push(d));
      proc.stderr?.on("data", (d) => chunks.push(d));
      proc.on("error", reject);
      proc.on("exit", async () => {
        let text = "";
        try {
          text = await readFile(lastMessageFile, "utf8");
        } catch {
          // Fallback to stdout JSONL parsing if the file is missing.
          const raw = Buffer.concat(chunks).toString("utf8");
          const lines = raw.split("\n").filter(Boolean);
          for (const line of lines) {
            try {
              const obj = JSON.parse(line) as Record<string, unknown>;
              if (typeof obj.text === "string") text += obj.text;
              if (typeof obj.message === "string") text += obj.message;
            } catch {
              text += line;
            }
          }
        }
        resolve({ text, usage: { lastMessageFile } });
      });
    });
  }
}
