import { type ChildProcess } from "node:child_process";
import { randomBytes } from "node:crypto";
import { createInterface } from "node:readline";
import { networkInterfaces } from "node:os";
import type { ServerInfo } from "../types.js";
import { WebSocket, WebSocketServer } from "ws";
import spawner from "../spawner.js";
import { loadMcpConfig, toAcpMcpServers } from "../mcp/mcp-config.js";

export interface ServeOptions {
  bind?: string;
  port?: number;
  secret?: string;
  cwd?: string;
  model?: string;
  yolo?: boolean;
}

const MAX_MESSAGE_BYTES = 10 * 1024 * 1024;
const MAX_CONNECTIONS_PER_IP = 10;
const CONNECTION_WINDOW_MS = 60_000;
const MAX_MESSAGES_PER_CONNECTION = 100;
const MESSAGE_WINDOW_MS = 10_000;

export function makeSecret(): string {
  return randomBytes(32).toString("hex").toUpperCase();
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

function isLocalOrigin(origin: string): boolean {
  if (!origin) return true;
  try {
    const u = new URL(origin);
    if (u.hostname === "localhost" || u.hostname === "127.0.0.1" || u.hostname === "::1") return true;
    if (u.protocol === "capacitor:" || u.protocol === "ionic:" || u.protocol === "file:") return true;
    if (u.protocol === "https:" && u.hostname.endsWith(".localhost")) return true;
    return false;
  } catch {
    return false;
  }
}

function isPrivateIp(ip: string): boolean {
  // Handle IPv4-mapped IPv6 addresses first.
  if (ip.startsWith("::ffff:")) {
    return isPrivateIp(ip.slice(7));
  }
  if (
    ip === "127.0.0.1" ||
    ip === "::1" ||
    ip.startsWith("10.") ||
    ip.startsWith("192.168.") ||
    /^172\.(1[6-9]|2[0-9]|3[0-1])\./.test(ip) ||
    ip.startsWith("fe80:") ||
    ip.startsWith("fc") ||
    ip.startsWith("fd")
  ) {
    return true;
  }
  return false;
}

function isOriginAllowed(origin: string, clientIp: string): boolean {
  // Only allow same-machine origins. The server-key provides the actual
  // authentication, but this prevents cross-site WebSocket abuse from
  // non-browser clients that still send an Origin header.
  if (!origin || isLocalOrigin(origin)) return true;
  console.warn(`[omgb serve] rejected origin '${origin}' from ${clientIp}`);
  return false;
}

interface Client {
  ws: any;
  proc: ChildProcess;
  stderrTail: string[];
  messageCount: number;
  messageWindowStart: number;
}

interface ConnectionRateEntry {
  count: number;
  windowStart: number;
}

export async function startAgentServer(options: ServeOptions = {}): Promise<ServerInfo & { process?: ChildProcess }> {
  const bind = options.bind ?? "127.0.0.1";
  const secret = options.secret ?? makeSecret();
  const cwd = options.cwd ?? process.cwd();

  const wss = new WebSocketServer({
    host: bind,
    port: options.port ?? 0,
    maxPayload: MAX_MESSAGE_BYTES,
  });

  const clients = new Set<Client>();
  const connectionRates = new Map<string, ConnectionRateEntry>();
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
    const clientIp = getClientIp(req);

    if (!checkConnectionRate(clientIp)) {
      ws.close(1008, "rate limit exceeded");
      return;
    }

    const origin = req.headers.origin ?? "";
    if (!isOriginAllowed(origin, clientIp)) {
      ws.close(1008, "origin not allowed");
      return;
    }

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

    const client: Client = { ws, proc, stderrTail: [], messageCount: 0, messageWindowStart: Date.now() };
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

    ws.on("message", async (data: unknown) => {
      if (!checkMessageRate(client)) {
        ws.close(1009, "message rate exceeded");
        return;
      }
      const buf = Buffer.isBuffer(data)
        ? data
        : Array.isArray(data)
          ? Buffer.concat(data)
          : Buffer.from(data as ArrayBuffer);
      if (buf.length > MAX_MESSAGE_BYTES) {
        ws.close(1009, "message too large");
        return;
      }
      if (!proc.stdin?.writable) return;
      const payload = await injectMcpServers(buf.toString());
      proc.stdin.write(payload + "\n");
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

  if (bind === "0.0.0.0") {
    console.warn(
      "[omgb serve] listening on all interfaces. Ensure you trust the network or use --bind 127.0.0.1 to restrict to localhost."
    );
  }

  const close = async (): Promise<void> => {
    for (const client of Array.from(clients)) {
      cleanupClient(client, 1000, "server closing");
    }
    connectionRates.clear();
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

  function getClientIp(req: import("http").IncomingMessage): string {
    const socket = (req as any).socket;
    const remoteAddress = socket?.remoteAddress ? String(socket.remoteAddress) : "unknown";

    // Only trust X-Forwarded-For from loopback. If a real reverse proxy is
    // in use, the operator can set --bind 127.0.0.1 and proxy to that.
    if (isPrivateIp(remoteAddress)) {
      const forwarded = req.headers["x-forwarded-for"];
      if (typeof forwarded === "string") {
        return forwarded.split(",")[0].trim();
      }
    }
    return remoteAddress;
  }

  function checkConnectionRate(ip: string): boolean {
    const now = Date.now();
    let entry = connectionRates.get(ip);
    if (!entry || now - entry.windowStart > CONNECTION_WINDOW_MS) {
      entry = { count: 0, windowStart: now };
    }
    entry.count++;
    connectionRates.set(ip, entry);
    return entry.count <= MAX_CONNECTIONS_PER_IP;
  }

  function checkMessageRate(client: Client): boolean {
    const now = Date.now();
    if (now - client.messageWindowStart > MESSAGE_WINDOW_MS) {
      client.messageWindowStart = now;
      client.messageCount = 0;
    }
    client.messageCount++;
    return client.messageCount <= MAX_MESSAGES_PER_CONNECTION;
  }

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
    // Prefer the Authorization header (Node clients).
    const auth = req.headers.authorization ?? "";
    const m = auth.match(/^Bearer\s+(.+)$/i);
    if (m) return m[1];

    // Browser-based WebViews cannot set arbitrary headers, but they can supply
    // the secret as a WebSocket subprotocol, which is sent in the handshake and
    // avoids putting it in the URL query string.
    const protocols = req.headers["sec-websocket-protocol"];
    if (typeof protocols === "string" && protocols.trim()) {
      const protocol = protocols.split(",")[0]?.trim();
      if (protocol) return protocol;
    }

    // Fallback for clients that can only pass the key in the URL query.
    // This is used for the initial QR-code pairing but should be avoided
    // for persistent connections.
    try {
      const fromUrl = new URL(req.url ?? "", "http://localhost").searchParams.get("server-key") ?? undefined;
      if (fromUrl) {
        console.warn("[omgb serve] client authenticated via URL query; prefer Authorization header or subprotocol");
      }
      return fromUrl;
    } catch {
      return undefined;
    }
  }
}

export function stopAgentServer(server: ServerInfo): Promise<void> {
  return server.close?.() ?? Promise.resolve();
}

async function injectMcpServers(raw: string): Promise<string> {
  let msg: Record<string, unknown>;
  try {
    msg = JSON.parse(raw) as Record<string, unknown>;
  } catch {
    return raw;
  }
  const method = msg.method as string | undefined;
  if (method !== "session/new" && method !== "session/load") {
    return raw;
  }
  const params = (msg.params ?? {}) as Record<string, unknown>;
  const active = toAcpMcpServers(await loadMcpConfig());
  const existing = Array.isArray(params.mcpServers) ? (params.mcpServers as Record<string, unknown>[]) : [];
  const seen = new Set(existing.map((s) => s.name).filter((n): n is string => typeof n === "string"));
  const merged: Record<string, unknown>[] = [...existing];
  for (const s of active) {
    if (!seen.has(s.name)) {
      merged.push(s as Record<string, unknown>);
      seen.add(s.name);
    }
  }
  params.mcpServers = merged;
  msg.params = params;
  return JSON.stringify(msg);
}
