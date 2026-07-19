import type {
  AcpAuthMethod,
  AcpInitializeResponse,
  AcpMessage,
  AcpNewSessionResponse,
  AcpPermissionRequest,
  AcpPermissionResponse,
  AcpPromptPart,
  AcpSessionConfigOption,
  AcpSetConfigOptionResponse,
  AcpUpdate,
} from "../types.js";

export type { AcpAuthMethod, AcpPermissionRequest, AcpPermissionResponse };

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
  private sessionConfigOptions = new Map<string, AcpSessionConfigOption[]>();

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

  async initialize(
    protocolVersion = 1,
    capabilities: Record<string, unknown> = {},
    timeoutMs = 120_000
  ): Promise<AcpInitializeResponse> {
    const result = (await this.request(
      "initialize",
      { protocolVersion, clientCapabilities: capabilities },
      timeoutMs
    )) as AcpInitializeResponse;
    this.initialized = true;
    return result;
  }

  async authenticate(authMethod: AcpAuthMethod, timeoutMs = 60_000): Promise<unknown> {
    return this.request("authenticate", { methodId: authMethod.id, _meta: { headless: true } }, timeoutMs);
  }

  async newSession(
    cwd: string,
    mcpServers: unknown[] = [],
    meta: Record<string, unknown> = {},
    timeoutMs = 120_000
  ): Promise<AcpNewSessionResponse> {
    const params: Record<string, unknown> = { cwd, mcpServers };
    if (Object.keys(meta).length) params._meta = meta;
    const response = (await this.request("session/new", params, timeoutMs)) as AcpNewSessionResponse;
    if (response.configOptions) {
      this.sessionConfigOptions.set(response.sessionId, response.configOptions);
    }
    return response;
  }

  private findConfigOption(sessionId: string, category: string): AcpSessionConfigOption | undefined {
    return this.sessionConfigOptions.get(sessionId)?.find((o) => o.category === category);
  }

  private updateConfigOptions(sessionId: string, configOptions: AcpSessionConfigOption[] | undefined): void {
    if (configOptions) {
      this.sessionConfigOptions.set(sessionId, configOptions);
    }
  }

  async setConfigOption(
    sessionId: string,
    configId: string,
    value: string | boolean,
    timeoutMs = 60_000
  ): Promise<AcpSetConfigOptionResponse> {
    const params: Record<string, unknown> = { sessionId, configId, value };
    if (typeof value === "boolean") {
      params.type = "boolean";
    }
    const response = (await this.request("session/set_config_option", params, timeoutMs)) as AcpSetConfigOptionResponse;
    this.updateConfigOptions(sessionId, response.configOptions);
    return response;
  }

  async setModel(
    sessionId: string,
    modelId: string,
    meta?: Record<string, unknown>,
    timeoutMs = 60_000
  ): Promise<unknown> {
    const modelOption = this.findConfigOption(sessionId, "model");
    if (modelOption) {
      return this.setConfigOption(sessionId, modelOption.id, modelId, timeoutMs);
    }
    const params: Record<string, unknown> = { sessionId, modelId };
    if (meta && Object.keys(meta).length) params._meta = meta;
    return this.request("session/set_model", params, timeoutMs);
  }

  async setModelWithEffort(
    sessionId: string,
    modelId: string,
    reasoningEffort: string,
    timeoutMs = 60_000
  ): Promise<unknown> {
    const modelOption = this.findConfigOption(sessionId, "model");
    if (modelOption) {
      await this.setConfigOption(sessionId, modelOption.id, modelId, timeoutMs);
      return this.setEffort(sessionId, reasoningEffort, timeoutMs);
    }
    const params: Record<string, unknown> = { sessionId, modelId, _meta: { reasoningEffort } };
    return this.request("session/set_model", params, timeoutMs);
  }

  async setEffort(sessionId: string, effort: string, timeoutMs = 60_000): Promise<boolean> {
    const options = this.sessionConfigOptions.get(sessionId) ?? [];
    const option =
      options.find((o) => o.category === "thought_level") ??
      options.find((o) => o.category === "model_config" && /effort|reason|thinking/i.test(`${o.id} ${o.name ?? ""}`));
    if (option) {
      await this.setConfigOption(sessionId, option.id, effort, timeoutMs);
      return true;
    }
    return false;
  }

  async setMode(sessionId: string, mode: string, timeoutMs = 60_000): Promise<boolean> {
    const option = this.findConfigOption(sessionId, "mode");
    if (option?.options?.some((o) => o.value === mode)) {
      await this.setConfigOption(sessionId, option.id, mode, timeoutMs);
      return true;
    }
    return false;
  }

  async prompt(sessionId: string, prompt: AcpPromptPart[], timeoutMs = 120_000): Promise<unknown> {
    return this.request("session/prompt", { sessionId, prompt }, timeoutMs);
  }

  async cancel(sessionId: string, timeoutMs = 30_000): Promise<unknown> {
    return this.request("session/cancel", { sessionId }, timeoutMs).catch(() => undefined);
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
        this.updateConfigOptions(params.sessionId, params.update.configOptions);
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
        if (p.sessionId && p.update) {
          this.updateConfigOptions(p.sessionId, p.update.configOptions);
          this.handlers.onUpdate?.(p.sessionId, p.update);
        }
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
        result = (await this.handlers.onPermission?.(req)) ?? { outcome: { outcome: "cancelled" } };
      } else if (method === "x.ai/ask_user_question") {
        const q = (params as { question: string }).question ?? "";
        const answer = await this.handlers.onAskUser?.({ question: q });
        result = { answer: answer ?? "" };
      } else {
        throw new Error(`Unsupported server request: ${method}`);
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
