import type { AcpMessage, AcpPermissionRequest, AcpPermissionResponse, AcpPromptPart, AcpUpdate } from "../types.js";

export type { AcpPermissionRequest, AcpPermissionResponse };

export interface AcpTransport {
  send(message: string): void;
  close(): void;
  onOpen?: () => void;
  onClose?: (code?: number, reason?: string) => void;
  onError?: (err: Error) => void;
  onMessage?: (message: string) => void;
}

export interface AcpClientHandlers {
  onUpdate?: (sessionId: string, update: AcpUpdate) => void;
  onPermission?: (req: AcpPermissionRequest) => Promise<AcpPermissionResponse>;
  onAskUser?: (params: { question: string }) => Promise<string | null>;
  onError?: (err: Error) => void;
  onClose?: () => void;
}

/**
 * A thin, JSON-RPC 2.0 ACP client for Grok Build's Agent Client Protocol.
 *
 * It is transport-agnostic: pass in a WebSocket-backed transport (Node `ws`
 * or browser `WebSocket`) and it handles requests, responses, notifications,
 * and incoming server->client requests such as `session/request_permission`.
 */
export class AcpClient {
  private transport: AcpTransport;
  private handlers: AcpClientHandlers;
  private pending = new Map<string | number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  private idCounter = 0;
  private initialized = false;

  constructor(transport: AcpTransport, handlers: AcpClientHandlers = {}) {
    this.transport = transport;
    this.handlers = handlers;
    this.transport.onMessage = (msg) => this.handleMessage(msg);
    this.transport.onError = (err) => handlers.onError?.(err);
    this.transport.onClose = () => handlers.onClose?.();
  }

  private nextId(): number {
    return ++this.idCounter;
  }

  private send(msg: AcpMessage): void {
    this.transport.send(JSON.stringify(msg));
  }

  private request(method: string, params: unknown, timeoutMs = 120_000): Promise<unknown> {
    const id = this.nextId();
    const msg: AcpMessage = { jsonrpc: "2.0", id, method, params };
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`ACP request '${method}' timed out after ${timeoutMs}ms`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (v: unknown) => {
          clearTimeout(timer);
          resolve(v);
        },
        reject: (e: Error) => {
          clearTimeout(timer);
          reject(e);
        },
      });
      this.send(msg);
    });
  }

  async initialize(protocolVersion = 1, capabilities: Record<string, unknown> = {}, timeoutMs = 120_000): Promise<unknown> {
    const result = await this.request("initialize", { protocolVersion, clientCapabilities: capabilities }, timeoutMs);
    this.initialized = true;
    return result;
  }

  async newSession(
    cwd: string,
    mcpServers: unknown[] = [],
    meta: Record<string, unknown> = {},
    timeoutMs = 120_000
  ): Promise<{ sessionId: string }> {
    const params: Record<string, unknown> = { cwd, mcpServers };
    if (Object.keys(meta).length) params._meta = meta;
    return (await this.request("session/new", params, timeoutMs)) as { sessionId: string };
  }

  async prompt(sessionId: string, prompt: AcpPromptPart[], timeoutMs = 120_000): Promise<unknown> {
    return this.request("session/prompt", { sessionId, prompt }, timeoutMs);
  }

  close(): void {
    this.transport.close();
  }

  private handleMessage(raw: string): void {
    let msg: AcpMessage;
    try {
      msg = JSON.parse(raw) as AcpMessage;
    } catch {
      this.handlers.onError?.(new Error(`Non-JSON ACP frame: ${raw.slice(0, 200)}`));
      return;
    }

    if (msg.id !== undefined && msg.id !== null) {
      if (msg.method) {
        // Server is requesting something from us.
        void this.handleServerRequest(msg);
        return;
      }
      // Response to a client request.
      const pending = this.pending.get(msg.id);
      if (!pending) return;
      this.pending.delete(msg.id);
      if (msg.error) {
        pending.reject(new Error(`ACP error ${msg.error.code}: ${msg.error.message}`));
      } else {
        pending.resolve(msg.result);
      }
      return;
    }

    if (msg.method && msg.method === "session/update") {
      const params = (msg.params ?? {}) as { sessionId?: string; update?: AcpUpdate };
      if (params.sessionId && params.update) {
        this.handlers.onUpdate?.(params.sessionId, params.update);
      }
      return;
    }

    // Wrapped x.ai extension notifications: top-level method starts with '_' and
    // the real method + params live nested.
    if (typeof msg.method === "string" && msg.method.startsWith("_")) {
      const wrapper = msg.params as { method?: string; params?: unknown } | undefined;
      if (wrapper?.method === "session/update" && wrapper.params) {
        const p = wrapper.params as { sessionId?: string; update?: AcpUpdate };
        if (p.sessionId && p.update) this.handlers.onUpdate?.(p.sessionId, p.update);
      }
    }
  }

  private async handleServerRequest(msg: AcpMessage): Promise<void> {
    const method = msg.method!;
    const params = msg.params ?? {};
    let result: unknown;
    try {
      if (method === "session/request_permission") {
        const req = params as AcpPermissionRequest;
        result = await this.handlers.onPermission?.(req) ?? { outcome: { outcome: "cancelled" } };
      } else if (method === "x.ai/ask_user_question") {
        const q = (params as { question: string }).question ?? "";
        const answer = await this.handlers.onAskUser?.({ question: q });
        result = { answer: answer ?? "" };
      } else {
        // Unknown server->client request: respond with empty object so the agent can continue.
        result = {};
      }
    } catch (err) {
      this.send({
        jsonrpc: "2.0",
        id: msg.id,
        error: {
          code: -32000,
          message: err instanceof Error ? err.message : String(err),
        },
      });
      return;
    }
    this.send({ jsonrpc: "2.0", id: msg.id, result });
  }
}
