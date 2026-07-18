export interface AcpPromptPart {
  type: "text" | "image";
  text?: string;
  source?: { type: "base64"; media_type: string; data: string };
}

export interface AcpSessionConfigOptionValue {
  value: string;
  name?: string;
  description?: string;
}

export interface AcpSessionConfigOption {
  id: string;
  name?: string;
  description?: string;
  category?: "model" | "mode" | "thought_level" | "model_config" | string;
  type?: "select" | "boolean" | string;
  currentValue?: string | boolean | unknown;
  options?: AcpSessionConfigOptionValue[];
  [key: string]: unknown;
}

export interface AcpUpdate {
  sessionUpdate:
    | "agent_message_chunk"
    | "agent_thought_chunk"
    | "tool_call"
    | "tool_call_update"
    | "turn_completed"
    | "stop"
    | "config_option_update"
    | "model_changed"
    | string;
  content?: { type: "text" | string; text?: string } | unknown;
  title?: string;
  status?: string;
  stopReason?: string;
  output?: unknown;
  configOptions?: AcpSessionConfigOption[];
  model_id?: string;
  reasoning_effort?: string;
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
    { outcome: "selected"; optionId: string } | { outcome: "cancelled" } | { outcome: string; optionId?: string };
}

export interface AcpHandlers {
  onUpdate?: (sessionId: string, update: AcpUpdate) => void;
  onPermission?: (req: AcpPermissionRequest) => Promise<AcpPermissionResponse>;
  onAskUser?: (question: string) => Promise<string | null>;
  onModelsUpdate?: (models: string[]) => void;
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

export interface AcpAuthMethod {
  id: string;
  [key: string]: unknown;
}

export interface AcpAgentInfo {
  name?: string;
  title?: string;
  version?: string;
  [key: string]: unknown;
}

export interface AcpAgentCapabilities {
  loadSession?: boolean | unknown;
  promptCapabilities?: Record<string, unknown>;
  mcpCapabilities?: Record<string, unknown>;
  auth?: { logout?: unknown };
  session?: { close?: unknown; list?: unknown; load?: unknown };
  [key: string]: unknown;
}

export interface AcpInitializeResponse {
  protocolVersion?: number;
  agentInfo?: AcpAgentInfo;
  agentCapabilities?: AcpAgentCapabilities;
  authMethods?: AcpAuthMethod[];
  meta?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface AcpNewSessionResponse {
  sessionId: string;
  configOptions?: AcpSessionConfigOption[];
  modes?: { id: string; name?: string }[];
  [key: string]: unknown;
}

export interface AcpLoadSessionResponse {
  sessionId: string;
  configOptions?: AcpSessionConfigOption[];
  [key: string]: unknown;
}

export interface AcpListSessionsResponse {
  sessions?: AcpSessionInfo[];
  nextCursor?: string | null;
}

export interface AcpSessionInfo {
  sessionId: string;
  cwd?: string;
  title?: string;
  updatedAt?: string;
  [key: string]: unknown;
}

export interface AcpSetConfigOptionResponse {
  configOptions?: AcpSessionConfigOption[];
  [key: string]: unknown;
}

function extractSecretAsProtocol(url: string): { cleanUrl: string; protocols?: string[] } {
  try {
    const u = new URL(url);
    const secret = u.searchParams.get("server-key");
    if (secret) {
      u.searchParams.delete("server-key");
      return { cleanUrl: u.toString(), protocols: [secret] };
    }
  } catch {
    // not a valid URL, pass through
  }
  return { cleanUrl: url };
}

function mapAvailableModels(raw: unknown): string[] {
  const p = raw as
    | {
        availableModels?: { modelId?: string; name?: string }[];
        currentModelId?: string;
        models?: string[];
      }
    | undefined;
  const models =
    p?.availableModels?.map((m) => m.modelId ?? m.name ?? "").filter((m): m is string => !!m) ??
    p?.models?.filter((m): m is string => !!m) ??
    [];
  return models;
}

/**
 * Browser ACP client. Uses the global `WebSocket` so it works inside a
 * Capacitor WebView without extra native dependencies.
 *
 * Pairing secrets are extracted from the `server-key` query parameter and
 * passed as a WebSocket subprotocol so they do not appear in access logs.
 */
export class AcpClient {
  private ws: WebSocket;
  private handlers: AcpHandlers;
  private pending = new Map<string | number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>();
  private idCounter = 0;
  private queue: AcpMessage[] = [];
  private initResult: AcpInitializeResponse | null = null;
  private sessionConfigOptions = new Map<string, AcpSessionConfigOption[]>();

  constructor(url: string, handlers: AcpHandlers = {}) {
    this.handlers = handlers;
    const { cleanUrl, protocols } = extractSecretAsProtocol(url);
    this.ws = new WebSocket(cleanUrl, protocols);
    this.ws.onopen = () => {
      this.flushQueue();
      handlers.onOpen?.();
    };
    this.ws.onclose = () => {
      this.queue = [];
      handlers.onClose?.();
    };
    this.ws.onerror = () => handlers.onError?.(new Error("WebSocket error"));
    this.ws.onmessage = (ev) => this.handleMessage(ev.data);
  }

  get readyState(): number {
    return this.ws.readyState;
  }

  close(): void {
    // Clear handlers before closing so stale callbacks cannot fire after the
    // client has been replaced (e.g. during reconnect).
    this.ws.onopen = null;
    this.ws.onclose = null;
    this.ws.onerror = null;
    this.ws.onmessage = null;
    this.ws.close();
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
    this.initResult = result;
    return result;
  }

  async authenticate(
    authMethod: AcpAuthMethod,
    extraMeta?: Record<string, unknown>,
    timeoutMs = 60_000
  ): Promise<unknown> {
    return this.request(
      "authenticate",
      { methodId: authMethod.id, _meta: { headless: true, ...extraMeta } },
      timeoutMs
    );
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
    if (response?.sessionId) {
      this.sessionConfigOptions.set(response.sessionId, response.configOptions ?? []);
    }
    return response;
  }

  async loadSession(
    sessionId: string,
    cwd = "/",
    mcpServers: unknown[] = [],
    timeoutMs = 120_000
  ): Promise<AcpLoadSessionResponse> {
    const response = (await this.request(
      "session/load",
      { sessionId, cwd, mcpServers },
      timeoutMs
    )) as AcpLoadSessionResponse;
    this.sessionConfigOptions.set(sessionId, response.configOptions ?? []);
    return response;
  }

  async listSessions(cursor?: string, timeoutMs = 30_000): Promise<AcpListSessionsResponse> {
    return (await this.request("session/list", { cursor: cursor ?? null }, timeoutMs)) as AcpListSessionsResponse;
  }

  async closeSession(sessionId: string, timeoutMs = 30_000): Promise<unknown> {
    if (!this.initResult?.agentCapabilities?.session?.close) {
      return Promise.resolve({});
    }
    return this.request("session/close", { sessionId }, timeoutMs).catch(() => undefined);
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
    if (response?.configOptions) {
      this.sessionConfigOptions.set(sessionId, response.configOptions);
    }
    return response;
  }

  private findConfigOption(sessionId: string, category: string): AcpSessionConfigOption | undefined {
    return this.sessionConfigOptions.get(sessionId)?.find((o) => o.category === category);
  }

  async setModel(sessionId: string, modelId: string, timeoutMs = 60_000): Promise<unknown> {
    const modelOption = this.findConfigOption(sessionId, "model");
    if (modelOption) {
      return this.setConfigOption(sessionId, modelOption.id, modelId, timeoutMs);
    }
    // Fallback for older agents that still expose session/set_model.
    return this.request("session/set_model", { sessionId, modelId }, timeoutMs);
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
    return this.request("session/set_model", { sessionId, modelId, _meta: { reasoningEffort } }, timeoutMs);
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

  sendRaw(message: AcpMessage): void {
    if (this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(message));
      return;
    }
    if (this.ws.readyState === WebSocket.CLOSING || this.ws.readyState === WebSocket.CLOSED) {
      throw new Error("WebSocket is closed");
    }
    this.queue.push(message);
  }

  private flushQueue(): void {
    for (const message of this.queue) {
      this.ws.send(JSON.stringify(message));
    }
    this.queue = [];
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

    const method = msg.method ?? "";

    if (method.startsWith("_")) {
      const wrapper = msg.params as { method?: string; params?: unknown } | undefined;
      if (wrapper?.method) {
        this.dispatchNotification(wrapper.method, wrapper.params);
      }
      return;
    }

    this.dispatchNotification(method, msg.params);
  }

  private dispatchNotification(method: string, params: unknown): void {
    if (method === "session/update" || method === "x.ai/session_notification") {
      const p = (params ?? {}) as { sessionId?: string; update?: AcpUpdate };
      if (p.sessionId && p.update) {
        if (p.update.configOptions) {
          this.sessionConfigOptions.set(p.sessionId, p.update.configOptions);
        }
        this.handlers.onUpdate?.(p.sessionId, p.update);
      }
      return;
    }

    if (method === "x.ai/models/update") {
      const models = mapAvailableModels(params);
      if (models.length) this.handlers.onModelsUpdate?.(models);
      return;
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
        const answer = await this.handlers.onAskUser?.(q);
        result = { answer: answer ?? "" };
      } else {
        throw new Error(`Unsupported server request: ${method}`);
      }
    } catch (err) {
      this.sendRaw({
        jsonrpc: "2.0",
        id: msg.id,
        error: {
          code: -32000,
          message: err instanceof Error ? err.message : String(err),
        },
      });
      return;
    }
    this.sendRaw({ jsonrpc: "2.0", id: msg.id, result });
  }
}
