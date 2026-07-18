import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { Connector, ConnectorConfig, ConnectorResult } from "./types.js";
import { sanitizeUserEnv } from "../env.js";
import { isRateLimited, formatRateLimitMessage } from "../rate-limit.js";
import spawner from "../spawner.js";

export class CodexConnector implements Connector {
  constructor(readonly config: ConnectorConfig) {}

  private getCodexEnv(): Record<string, string> {
    const safe = sanitizeUserEnv(this.config.env);
    if (this.config.secret && !safe.CODEX_API_KEY && !safe.OPENAI_API_KEY) {
      safe.CODEX_API_KEY = this.config.secret;
    }
    if (safe.OPENAI_API_KEY && !safe.CODEX_API_KEY) {
      safe.CODEX_API_KEY = safe.OPENAI_API_KEY;
    }
    return safe;
  }

  private getApiKey(): string | undefined {
    const safe = this.getCodexEnv();
    return safe.CODEX_API_KEY ?? safe.OPENAI_API_KEY;
  }

  private codexEnv(): Record<string, string | undefined> {
    return { ...process.env, ...this.getCodexEnv() };
  }

  private async isLoggedIn(): Promise<boolean> {
    return new Promise((resolve) => {
      const proc = spawner.spawn("codex", ["login", "status"], {
        cwd: this.config.cwd ?? process.cwd(),
        env: this.codexEnv(),
        stdio: ["ignore", "pipe", "pipe"],
      });
      proc.on("exit", (code) => resolve(code === 0));
      proc.on("error", () => resolve(false));
    });
  }

  private async login(): Promise<void> {
    const key = this.getApiKey();
    if (!key) return;
    if (await this.isLoggedIn()) return;

    const stderr: Buffer[] = [];
    await new Promise<void>((resolve, reject) => {
      const proc = spawner.spawn("codex", ["login", "--with-api-key"], {
        cwd: this.config.cwd ?? process.cwd(),
        env: this.codexEnv(),
        stdio: ["pipe", "pipe", "pipe"],
      });
      proc.stderr?.on("data", (d: Buffer) => stderr.push(d));
      proc.on("error", reject);
      proc.stdin?.write(key);
      proc.stdin?.end();
      proc.on("exit", (code) => {
        if (code === 0) {
          resolve();
        } else {
          const err = Buffer.concat(stderr).toString("utf8").slice(-500);
          reject(new Error(`codex login failed with code ${code}${err ? `: ${err}` : ""}`));
        }
      });
    });
  }

  async run(prompt: string): Promise<ConnectorResult> {
    await this.login();

    const tmpDir = await mkdtemp(join(tmpdir(), "omgb-codex-"));
    const lastMessageFile = join(tmpDir, "last-message.txt");

    const args = ["exec", "--json", "--sandbox", "workspace-write", "--output-last-message", lastMessageFile, prompt];

    return new Promise<ConnectorResult>((resolve, reject) => {
      const chunks: Buffer[] = [];
      const proc = spawner.spawn("codex", args, {
        cwd: this.config.cwd ?? process.cwd(),
        env: this.codexEnv(),
        stdio: ["ignore", "pipe", "pipe"],
      });

      proc.stdout?.on("data", (d) => chunks.push(d));
      proc.stderr?.on("data", (d) => chunks.push(d));
      proc.on("error", (err) => {
        void rm(tmpDir, { recursive: true, force: true });
        reject(err);
      });
      proc.on("exit", async (code) => {
        try {
          if (code !== 0 && code !== null) {
            const stderrText = Buffer.concat(chunks).toString("utf8");
            if (isRateLimited(stderrText)) {
              reject(new Error(formatRateLimitMessage()));
            } else {
              const snippet = stderrText.slice(-500);
              reject(new Error(`codex exited with code ${code}${snippet ? `: ${snippet}` : ""}`));
            }
            return;
          }

          let text = "";
          try {
            text = await readFile(lastMessageFile, "utf8");
          } catch {
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
        } finally {
          void rm(tmpDir, { recursive: true, force: true });
        }
      });
    });
  }
}
