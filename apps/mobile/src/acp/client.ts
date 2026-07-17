export interface AcpPromptPart {
  type: "text" | "image";
  text?: string;
  source?: { type: "base64"; media_type: string; data: string };
}

export interface AcpUpdate {
  sessionUpdate:
    | "agent_message_chunk"
    | "agent_thought_chunk"
    | "tool_call"
    | "tool_call_update"
    | "turn_completed"
    | "stop"
    | string;
  content?: { type: "text" | string; text?: string } | unknown;
  title?: string;
  status?: string;
  stopReason?: string;
  [key: string]: unknown;
}

export interface AcpPermissionOption {
  optionId: string;
  name: string;
  kind?: string;
}

export interface AcpPermissionRequest {
  sessionId: string;
  toolCall: {
    toolCallId: string;
    title?: string;
    command?: string;
    [key: string]: unknown;
  };
  options: AcpPermissionOption[];
}

export interface AcpPermissionResponse {
  outcome:
    | { outcome: "selected"; optionId: string }
    | { outcome: "cancelled" }
    | { outcome: string; optionId?: string };
}

export interface AcpHandlers {
  onUpdate?: (sessionId: string, update: AcpUpdate) => void;
  onPermission?: (req: AcpPermissionRequest) => Promise<AcpPermissionResponse>;
  onAskUser?: (question: string) => Promise<string | null>;
  onError?: (err: Error) => void;
  onClose?: () => void;
  onOpen?: () => void;
}

export interface AcpMessage {
  jsonrpc: "2.0";
  id?: number | string | null;
  method?: string;
  params?: unknown;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
}

/**
 * Browser ACP client. Uses the global `WebSocket` so it works inside a
 * Capacitor WebView without extra native dependencies.
 */
export class AcpClient {
  private ws: WebSocket;
  private handlers: AcpHandlers;
  private pending = new Map<string | number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  private idCounter = 0;

  constructor(url: string, handlers: AcpHandlers = {}) {
    this.handlers = handlers;
    this.ws = new WebSocket(url);
    this.ws.onopen = () => handlers.onOpen?.();
    this.ws.onclose = () => handlers.onClose?.();
    this.ws.onerror = () => handlers.onError?.(new Error("WebSocket error"));
    this.ws.onmessage = (ev) => this.handleMessage(ev.data);
  }

  get readyState(): number {
    return this.ws.readyState;
  }

  close(): void {
    this.ws.close();
  }

  async initialize(
    protocolVersion = 1,
    capabilities: Record<string, unknown> = {},
    timeoutMs = 120_000
  ): Promise<unknown> {
    return this.request("initialize", { protocolVersion, clientCapabilities: capabilities }, timeoutMs);
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

  sendRaw(message: AcpMessage): void {
    this.ws.send(JSON.stringify(message));
  }

  private nextId(): number {
    return ++this.idCounter;
  }

  private request(method: string, params: unknown, timeoutMs = 120_000): Promise<unknown> {
    const id = this.nextId();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`ACP request '${method}' timed out`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (v) => {
          clearTimeout(timer);
          resolve(v);
        },
        reject: (e) => {
          clearTimeout(timer);
          reject(e);
        },
      });
      this.sendRaw({ jsonrpc: "2.0", id, method, params });
    });
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
        void this.handleServerRequest(msg);
        return;
      }
      const pending = this.pending.get(msg.id);
      if (!pending) return;
      this.pending.delete(msg.id);
      if (msg.error) pending.reject(new Error(`${msg.error.code}: ${msg.error.message}`));
      else pending.resolve(msg.result);
      return;
    }

    if (msg.method === "session/update") {
      const p = (msg.params ?? {}) as { sessionId?: string; update?: AcpUpdate };
      if (p.sessionId && p.update) this.handlers.onUpdate?.(p.sessionId, p.update);
      return;
    }

    if (typeof msg.method === "string" && msg.method.startsWith("_")) {
      const wrapper = msg.params as { method?: string; params?: { sessionId?: string; update?: AcpUpdate } } | undefined;
      if (wrapper?.method === "session/update" && wrapper.params?.sessionId && wrapper.params.update) {
        this.handlers.onUpdate?.(wrapper.params.sessionId, wrapper.params.update);
      }
    }
  }

  private async handleServerRequest(msg: AcpMessage): Promise<void> {
    const method = msg.method!;
    const params = msg.params ?? {};
    let result: unknown;
    try {
      if (method === "session/request_permission") {
        result = await this.handlers.onPermission?.(params as AcpPermissionRequest) ?? { outcome: { outcome: "cancelled" } };
      } else if (method === "x.ai/ask_user_question") {
        const q = (params as { question: string }).question ?? "";
        const answer = await this.handlers.onAskUser?.(q);
        result = { answer: answer ?? "" };
      } else {
        result = {};
      }
    } catch (err) {
      this.sendRaw({ jsonrpc: "2.0", id: msg.id, error: { code: -32000, message: err instanceof Error ? err.message : String(err) } });
      return;
    }
    this.sendRaw({ jsonrpc: "2.0", id: msg.id, result });
  }
}
