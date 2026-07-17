import { spawn, type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createServer } from "node:net";
import { networkInterfaces } from "node:os";
import type { ServerInfo } from "../types.js";

export interface ServeOptions {
  bind?: string;
  port?: number;
  secret?: string;
  cwd?: string;
  model?: string;
  yolo?: boolean;
}

function findFreePort(preferred?: number): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.listen(preferred ?? 0, "127.0.0.1", () => {
      const addr = server.address();
      const port = typeof addr === "object" && addr ? addr.port : 0;
      server.close(() => resolve(port));
    });
    server.on("error", reject);
  });
}

export function makeSecret(): string {
  return randomBytes(16).toString("hex").toUpperCase();
}

export function parseServerUrl(url?: string): { host: string; port: number; secret?: string } {
  if (!url) return { host: "127.0.0.1", port: 0 };
  const m = url.match(/^wss?:\/\/([^/:]+)(?::(\d+))?/);
  const secret = new URL(url).searchParams.get("server-key") ?? undefined;
  if (!m) throw new Error(`Invalid WebSocket URL: ${url}`);
  return { host: m[1], port: m[2] ? parseInt(m[2], 10) : 0, secret };
}

export function formatServerUrl(host: string, port: number, secret: string): string {
  return `ws://${host}:${port}/ws?server-key=${encodeURIComponent(secret)}`;
}

export function getLocalIp(): string | undefined {
  const interfaces = networkInterfaces();
  for (const [, addrs] of Object.entries(interfaces)) {
    for (const addr of addrs ?? []) {
      if (addr.family === "IPv4" && !addr.internal && !addr.address.startsWith("169.254")) {
        return addr.address;
      }
    }
  }
  return undefined;
}

export async function startAgentServer(options: ServeOptions = {}): Promise<ServerInfo & { process: ChildProcess }> {
  const bind = options.bind ?? "0.0.0.0";
  const port = options.port ?? (await findFreePort());
  const secret = options.secret ?? makeSecret();
  const cwd = options.cwd ?? process.cwd();

  const args = ["agent", "serve", "--bind", `${bind}:${port}`, "--secret", secret];
  if (options.model) args.push("--model", options.model);
  if (options.yolo) args.push("--yolo");

  const proc = spawn("grok", args, {
    cwd,
    stdio: ["ignore", "pipe", "pipe"],
    env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
  });

  // Wait for the "listening" log line or process exit.
  const ready = await new Promise<void>((resolve, reject) => {
    const onData = (data: Buffer) => {
      const line = data.toString();
      if (line.includes("Agent server listening")) {
        cleanup();
        resolve();
      } else if (line.includes("error") || line.includes("Error")) {
        cleanup();
        reject(new Error(`grok agent serve failed: ${line.trim()}`));
      }
    };
    const onExit = (code: number | null) => {
      cleanup();
      reject(new Error(`grok agent serve exited early with code ${code}`));
    };
    const cleanup = () => {
      proc.stdout?.off("data", onData);
      proc.stderr?.off("data", onData);
      proc.off("exit", onExit);
    };
    proc.stdout?.on("data", onData);
    proc.stderr?.on("data", onData);
    proc.on("exit", onExit);

    // Fallback: resolve after 5s if no error.
    setTimeout(() => {
      cleanup();
      resolve();
    }, 5000);
  });

  // Promise resolved when the server is listening or fallback timer fires.
  void ready;

  const hostForClient = bind === "0.0.0.0" ? (getLocalIp() ?? bind) : bind;
  return {
    url: formatServerUrl(hostForClient, port, secret),
    secret,
    pid: proc.pid,
    cwd,
    process: proc,
  };
}

export function stopAgentServer(proc: ChildProcess): Promise<void> {
  return new Promise((resolve) => {
    if (proc.killed || proc.exitCode !== null) {
      resolve();
      return;
    }
    proc.on("exit", () => resolve());
    proc.kill("SIGTERM");
    setTimeout(() => {
      if (!proc.killed && proc.exitCode === null) proc.kill("SIGKILL");
    }, 3000);
  });
}
