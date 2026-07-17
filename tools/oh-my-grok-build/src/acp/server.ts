import { type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createInterface } from "node:readline";
import { networkInterfaces } from "node:os";
import type { ServerInfo } from "../types.js";
import { WebSocket, WebSocketServer } from "ws";
import spawner from "../spawner.js";

export interface ServeOptions {
  bind?: string;
  port?: number;
  secret?: string;
  cwd?: string;
  model?: string;
  yolo?: boolean;
}

export function makeSecret(): string {
  return randomBytes(16).toString("hex").toUpperCase();
}

export interface ParsedServerUrl {
  host: string;
  port: number;
  secret?: string;
  baseUrl: string;
}

export function parseServerUrl(url?: string): ParsedServerUrl {
  if (!url) return { host: "127.0.0.1", port: 0, baseUrl: "" };
  const u = new URL(url);
  const secret = u.searchParams.get("server-key") ?? undefined;
  u.searchParams.delete("server-key");
  const baseUrl = u.toString();
  const m = baseUrl.match(/^wss?:\/\/([^/:]+)(?::(\d+))?/);
  if (!m) throw new Error(`Invalid WebSocket URL: ${url}`);
  return { host: m[1], port: m[2] ? parseInt(m[2], 10) : 0, secret, baseUrl };
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

interface Client {
  ws: any;
  proc: ChildProcess;
  stderrTail: string[];
}

export async function startAgentServer(options: ServeOptions = {}): Promise<ServerInfo & { process?: ChildProcess }> {
  const bind = options.bind ?? "0.0.0.0";
  const secret = options.secret ?? makeSecret();
  const cwd = options.cwd ?? process.cwd();

  const wss = new WebSocketServer({ host: bind, port: options.port ?? 0 });

  const clients = new Set<Client>();
  let address: { port: number; address: string } | null = null;

  const getHostForClient = () => {
    if (bind === "0.0.0.0") return getLocalIp() ?? "127.0.0.1";
    return bind;
  };

  const serverReady = new Promise<void>((resolve, reject) => {
    wss.on("listening", () => {
      const addr = wss.address() as any;
      if (addr && typeof addr === "object") {
        address = { port: (addr as any).port as number, address: (addr as any).address as string };
      }
      resolve();
    });
    wss.on("error", reject);
  });

  wss.on("connection", (ws: any, req: any) => {
    const clientSecret = extractSecret(ws, req);
    if (clientSecret !== secret) {
      console.warn("[omgb serve] rejected client: invalid server-key");
      ws.close(1008, "invalid server-key");
      return;
    }

    const args = ["agent", "stdio"];
    if (options.model) args.push("--model", options.model);
    if (options.yolo) args.push("--yolo");

    const proc = spawner.spawn("grok", args, {
      cwd,
      stdio: ["pipe", "pipe", "pipe"],
      env: { ...process.env, GROK_DISABLE_AUTOUPDATER: "1" },
    });

    const client: Client = { ws, proc, stderrTail: [] };
    clients.add(client);

    proc.on("error", (err) => {
      console.error(`[omgb serve] grok agent stdio error: ${err.message}`);
      cleanupClient(client, 1011, "agent spawn error");
    });

    proc.on("exit", (code) => {
      if (code !== 0 && code !== null) {
        const tail = client.stderrTail.slice(-10).join("");
        console.error(`[omgb serve] grok agent stdio exited with ${code}${tail ? `\n${tail}` : ""}`);
      }
      cleanupClient(client, 1011, "agent exited");
    });

    if (proc.stderr) {
      proc.stderr.on("data", (data: Buffer) => {
        const line = data.toString();
        client.stderrTail.push(line);
        if (client.stderrTail.length > 100) client.stderrTail.shift();
      });
    }

    const rl = createInterface({ input: proc.stdout! });
    rl.on("line", (line) => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(line);
      }
    });

    ws.on("message", (data: unknown) => {
      const buf = Buffer.isBuffer(data)
        ? data
        : Array.isArray(data)
          ? Buffer.concat(data)
          : Buffer.from(data as ArrayBuffer);
      if (proc.stdin?.writable) {
        proc.stdin.write(buf.toString() + "\n");
      }
    });

    ws.on("close", () => {
      cleanupClient(client);
    });

    ws.on("error", (err: any) => {
      console.error(`[omgb serve] WebSocket error: ${err.message}`);
      cleanupClient(client, 1011, "websocket error");
    });
  });

  await serverReady;

  const hostForClient = getHostForClient();
  const actualPort = (address as any)?.port ?? 0;

  const close = async (): Promise<void> => {
    for (const client of Array.from(clients)) {
      cleanupClient(client, 1000, "server closing");
    }
    return new Promise((resolve, reject) => {
      wss.close((err: Error | undefined) => (err ? reject(err) : resolve()));
    });
  };

  return {
    url: formatServerUrl(hostForClient, actualPort, secret),
    secret,
    cwd,
    close,
  };

  function cleanupClient(client: Client, code?: number, reason?: string): void {
    if (!clients.has(client)) return;
    clients.delete(client);
    try {
      if (client.ws.readyState === WebSocket.OPEN) {
        client.ws.close(code ?? 1000, reason ?? "closing");
      } else if (client.ws.readyState === WebSocket.CONNECTING) {
        client.ws.terminate();
      }
    } catch {
      // ignore
    }
    if (!client.proc.killed && client.proc.exitCode === null) {
      client.proc.kill("SIGTERM");
      setTimeout(() => {
        if (!client.proc.killed && client.proc.exitCode === null) {
          client.proc.kill("SIGKILL");
        }
      }, 3000);
    }
  }

  function extractSecret(_ws: WebSocket, req: import("http").IncomingMessage): string | undefined {
    // Prefer the Authorization header when available (Node clients). Browsers/WebViews that
    // cannot set headers continue to pass the key in the URL query as a fallback.
    const auth = req.headers.authorization ?? "";
    const m = auth.match(/^Bearer\s+(.+)$/i);
    if (m) return m[1];

    try {
      return new URL(req.url ?? "", "http://localhost").searchParams.get("server-key") ?? undefined;
    } catch {
      return undefined;
    }
  }
}

export function stopAgentServer(server: ServerInfo): Promise<void> {
  return server.close?.() ?? Promise.resolve();
}
