import type {
  AcpMessage,
  AcpPermissionOption,
  AcpPermissionRequest,
  AcpPermissionResponse,
  AcpSessionConfigOption,
  AcpUpdate,
} from "./types.js";

export interface AcpClientHandlers {
  onOpen?: () => void;
  onClose?: () => void;
  onError?: (err: Error) => void;
  onUpdate?: (sessionId: string, update: AcpUpdate) => void;
  onAskUser?: (question: string) => Promise<string>;
}

export interface AcpClientOptions {
  url: string;
  handlers?: AcpClientHandlers;
  yolo?: boolean;
}

const ALLOW_ONCE = ["allow_once", "allow-once", "approve_once"];
const ALLOW_ALWAYS = ["allow_always", "allow-always", "approve_always"];
const ALLOW = ["allow", "approve"];

function matchesKind(option: AcpPermissionOption, kinds: string[]): boolean {
  const kind = (option.kind ?? "").toLowerCase();
  const id = (option.optionId ?? "").toLowerCase();
  return kinds.some(
    (k) =>
      kind === k ||
      kind.startsWith(`${k}_`) ||
      kind.startsWith(`${k}-`) ||
      new RegExp(`(^|[-_.])${k}($|[-_.])`, "i").test(id),
  );
}

function selectPermissionOption(
  options: AcpPermissionOption[],
  yolo: boolean,
): string | undefined {
  if (yolo) {
    const always = options.find((o) => matchesKind(o, ALLOW_ALWAYS));
    if (always) return always.optionId;
  }
  const once = options.find((o) => matchesKind(o, ALLOW_ONCE));
  if (once) return once.optionId;
  const any = options.find((o) => matchesKind(o, ALLOW));
  return any?.optionId;
}

export class AcpClient {
  private ws: WebSocket | null = null;
  private pending = new Map<
    string | number,
    { resolve: (value: unknown) => void; reject: (err: Error) => void }
  >();
  private turnResolvers = new Map<
    string,
    {
      resolve: () => void;
      reject: (err: Error) => void;
      timer: ReturnType<typeof setTimeout>;
    }
  >();
  private nextId = 1;
  private handlers: AcpClientHandlers;
  private yolo: boolean;
  private sessionConfigOptions = new Map<string, AcpSessionConfigOption[]>();

  constructor(options: AcpClientOptions) {
    this.handlers = options.handlers ?? {};
    this.yolo = options.yolo ?? false;
    this.ws = new WebSocket(options.url);
    this.ws.addEventListener("open", () => this.handlers.onOpen?.());
    this.ws.addEventListener("message", (ev) => this.handleMessage(ev.data));
    this.ws.addEventListener("error", () =>
      this.handlers.onError?.(new Error("WebSocket error")),
    );
    this.ws.addEventListener("close", () => {
      for (const [, t] of this.turnResolvers) {
        clearTimeout(t.timer);
        t.reject(new Error("Connection closed"));
      }
      this.turnResolvers.clear();
      this.handlers.onClose?.();
    });
  }

  ready(): Promise<void> {
    return new Promise((resolve, reject) => {
      if (!this.ws) return reject(new Error("WebSocket closed"));
      if (this.ws.readyState === WebSocket.OPEN) return resolve();
      const onOpen = () => cleanup() && resolve();
      const onError = () => cleanup() && reject(new Error("WebSocket error"));
      const onClose = () =>
        cleanup() && reject(new Error("WebSocket closed before open"));
      const cleanup = () => {
        this.ws?.removeEventListener("open", onOpen);
        this.ws?.removeEventListener("error", onError);
        this.ws?.removeEventListener("close", onClose);
        return true;
      };
      this.ws.addEventListener("open", onOpen);
      this.ws.addEventListener("error", onError);
      this.ws.addEventListener("close", onClose);
    });
  }

  close(): void {
    this.ws?.close();
    this.ws = null;
  }

  private handleMessage(data: unknown): void {
    if (typeof data !== "string") return;
    let msg: AcpMessage;
    try {
      msg = JSON.parse(data) as AcpMessage;
    } catch {
      return;
    }

    if (msg.id !== undefined && msg.id !== null) {
      if (msg.method) {
        void this.handleServerRequest(msg);
        return;
      }
      const pending = this.pending.get(msg.id);
      if (pending) {
        this.pending.delete(msg.id);
        if (msg.error) pending.reject(new Error(msg.error.message));
        else pending.resolve(msg.result ?? {});
      }
      return;
    }

    if (msg.method === "session/update") {
      this.handleUpdate(
        (msg.params ?? {}) as { sessionId?: string; update?: AcpUpdate },
      );
      return;
    }

    // Some servers wrap notifications in an extension envelope.
    if (typeof msg.method === "string" && msg.method.startsWith("_")) {
      const wrapper = (msg.params ?? {}) as {
        method?: string;
        params?: { sessionId?: string; update?: AcpUpdate };
      };
      if (wrapper.method === "session/update" && wrapper.params) {
        this.handleUpdate(wrapper.params);
      }
    }
  }

  private handleUpdate(params: {
    sessionId?: string;
    update?: AcpUpdate;
  }): void {
    if (!params.sessionId || !params.update) return;
    this.handlers.onUpdate?.(params.sessionId, params.update);
    const state = params.update.sessionUpdate;
    if (state === "turn_completed" || state === "stop") {
      const t = this.turnResolvers.get(params.sessionId);
      if (t) {
        clearTimeout(t.timer);
        this.turnResolvers.delete(params.sessionId);
        t.resolve();
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
        const optionId = selectPermissionOption(req.options ?? [], this.yolo);
        const permission: AcpPermissionResponse = optionId
          ? { outcome: { outcome: "selected", optionId } }
          : { outcome: { outcome: "cancelled" } };
        result = permission;
      } else if (method === "x.ai/ask_user_question") {
        const q = (params as { question?: string }).question ?? "";
        const answer = await (this.handlers.onAskUser?.(q) ??
          Promise.resolve(""));
        result = { answer };
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

  private send(msg: AcpMessage): Promise<void> {
    return new Promise((resolve, reject) => {
      if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
        reject(new Error("WebSocket not open"));
        return;
      }
      try {
        this.ws.send(JSON.stringify(msg));
        resolve();
      } catch (err) {
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    });
  }

  private request(
    method: string,
    params: unknown,
    timeoutMs = 120_000,
  ): Promise<unknown> {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        if (this.pending.delete(id)) {
          reject(new Error(`Request ${method} timed out after ${timeoutMs}ms`));
        }
      }, timeoutMs);
      const cleanupResolve = (value: unknown) => {
        clearTimeout(timer);
        resolve(value);
      };
      const cleanupReject = (err: Error) => {
        clearTimeout(timer);
        reject(err);
      };
      this.pending.set(id, { resolve: cleanupResolve, reject: cleanupReject });
      void this.send({ jsonrpc: "2.0", id, method, params }).catch((err) => {
        if (this.pending.delete(id)) {
          cleanupReject(err instanceof Error ? err : new Error(String(err)));
        }
      });
    });
  }

  initialize(
    capabilities: Record<string, unknown> = {},
    timeoutMs = 120_000,
  ): Promise<{ authMethods?: { id: string }[] }> {
    return this.request(
      "initialize",
      {
        protocolVersion: 1,
        capabilities,
      },
      timeoutMs,
    ) as Promise<{ authMethods?: { id: string }[] }>;
  }

  authenticate(methodId: string, timeoutMs = 60_000): Promise<unknown> {
    return this.request(
      "authenticate",
      {
        methodId,
        _meta: { headless: true },
      },
      timeoutMs,
    );
  }

  async newSession(
    cwd: string,
    mcpServers: unknown[] = [],
    timeoutMs = 120_000,
  ): Promise<{ sessionId: string; configOptions?: AcpSessionConfigOption[] }> {
    const response = (await this.request(
      "session/new",
      {
        cwd,
        mcpServers,
        _meta: { yoloMode: this.yolo },
      },
      timeoutMs,
    )) as { sessionId: string; configOptions?: AcpSessionConfigOption[] };
    if (response.configOptions) {
      this.sessionConfigOptions.set(response.sessionId, response.configOptions);
    }
    return response;
  }

  private findConfigOption(
    sessionId: string,
    category: string,
  ): AcpSessionConfigOption | undefined {
    return this.sessionConfigOptions
      .get(sessionId)
      ?.find((o) => o.category === category);
  }

  private async setConfigOption(
    sessionId: string,
    configId: string,
    value: string | boolean,
    timeoutMs = 60_000,
  ): Promise<void> {
    const params: Record<string, unknown> = { sessionId, configId, value };
    if (typeof value === "boolean") params.type = "boolean";
    await this.request("session/set_config_option", params, timeoutMs);
  }

  async setMode(sessionId: string, mode: string): Promise<boolean> {
    const option = this.findConfigOption(sessionId, "mode");
    if (option?.options?.some((o) => o.value === mode)) {
      await this.setConfigOption(sessionId, option.id, mode);
      return true;
    }
    return false;
  }

  async setModel(
    sessionId: string,
    modelId: string,
    meta: Record<string, unknown> = {},
    timeoutMs = 60_000,
  ): Promise<unknown> {
    const modelOption = this.findConfigOption(sessionId, "model");
    if (modelOption) {
      return this.setConfigOption(
        sessionId,
        modelOption.id,
        modelId,
        timeoutMs,
      );
    }
    const params: Record<string, unknown> = { sessionId, modelId };
    if (Object.keys(meta).length) params._meta = meta;
    return this.request("session/set_model", params, timeoutMs);
  }

  async setEffort(
    sessionId: string,
    reasoningEffort: string,
    timeoutMs = 60_000,
  ): Promise<boolean> {
    const effortOption = this.findConfigOption(sessionId, "thought_level");
    if (effortOption) {
      await this.setConfigOption(
        sessionId,
        effortOption.id,
        reasoningEffort,
        timeoutMs,
      );
      return true;
    }
    return false;
  }

  prompt(
    sessionId: string,
    prompt: { type: string; text: string }[],
    timeoutMs = 120_000,
  ): Promise<void> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.turnResolvers.delete(sessionId);
        reject(new Error("Prompt timed out waiting for turn completion"));
      }, timeoutMs);
      this.turnResolvers.set(sessionId, {
        resolve: () => {
          clearTimeout(timer);
          this.turnResolvers.delete(sessionId);
          resolve();
        },
        reject: (err) => {
          clearTimeout(timer);
          this.turnResolvers.delete(sessionId);
          reject(err);
        },
        timer,
      });
      void this.request("session/prompt", { sessionId, prompt }).catch(
        (err) => {
          clearTimeout(timer);
          this.turnResolvers.delete(sessionId);
          reject(err);
        },
      );
    });
  }

  runTerminalCmd(
    args: string[],
    timeoutMs = 120_000,
  ): Promise<{ output: string; exitCode: number }> {
    return this.request(
      "x.ai/run_terminal_cmd",
      { args },
      timeoutMs,
    ) as Promise<{ output: string; exitCode: number }>;
  }
}
