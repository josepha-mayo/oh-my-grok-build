import { createServer } from "http";
import { WebSocketServer } from "ws";
import { randomBytes } from "crypto";
import { runGrok } from "./grok.js";
import type { GrokEvent, RelayClient } from "./types.js";

const clients = new Map<string, RelayClient>();

function makeCode(): string {
  return process.env.AGENTHUB_RELAY_CODE?.toUpperCase() ?? randomBytes(4).toString("hex").toUpperCase();
}

function send(ws: RelayClient["ws"], msg: object): void {
  try { ws.send(JSON.stringify(msg)); } catch { /* ignore closed */ }
}

export function startRelay(port = 3001): void {
  const httpServer = createServer((req, res) => {
    if (req.url === "/") {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ status: "oh-my-grok-build relay", port }));
      return;
    }
    res.writeHead(404);
    res.end("Not found");
  });

  const wss = new WebSocketServer({ server: httpServer });

  wss.on("connection", (ws, req) => {
    const code = makeCode();
    const client: RelayClient = { id: randomBytes(8).toString("hex"), code, ws, cwd: process.cwd() };
    clients.set(client.id, client);

    console.log(`[relay] client ${client.id} connected — code: ${code}`);
    send(ws, { type: "connected", id: client.id, code, message: "Connected to omgb relay." });

    ws.on("message", (data) => {
      let msg: Record<string, unknown>;
      try {
        msg = JSON.parse(data.toString("utf8"));
      } catch {
        send(ws, { type: "error", message: "Invalid JSON" });
        return;
      }

      if (msg.type === "set-cwd") {
        client.cwd = String(msg.cwd ?? process.cwd());
        send(ws, { type: "status", message: `CWD set to ${client.cwd}` });
        return;
      }

      if (msg.type === "prompt") {
        const prompt = String(msg.prompt ?? "");
        if (!prompt) {
          send(ws, { type: "error", message: "Missing prompt" });
          return;
        }
        const model = msg.model ? String(msg.model) : undefined;
        const yolo = Boolean(msg.yolo);
        send(ws, { type: "turn-started", prompt, model });
        const child = runGrok(prompt, { cwd: client.cwd, model, yolo, outputFormat: "streaming-json" });
        child.on("event", (ev: GrokEvent) => send(ws, { ...ev, turn: client.id }));
        child.on("end", () => send(ws, { type: "turn-ended", turn: client.id }));
        return;
      }

      send(ws, { type: "error", message: `Unknown type: ${msg.type}` });
    });

    ws.on("close", () => clients.delete(client.id));
  });

  httpServer.listen(port, () => {
    console.log(`oh-my-grok-build relay listening on ws://localhost:${port}`);
    console.log(`HTTP health: http://localhost:${port}/`);
  });
}
