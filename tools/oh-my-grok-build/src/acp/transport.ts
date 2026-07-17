import type { AcpTransport } from "./client.js";

export interface WebSocketLike {
  send(data: string): void;
  close(code?: number, reason?: string): void;
  readyState: number;
  onopen: ((ev: Event) => void) | null;
  onclose: ((ev: CloseEvent | { code: number; reason: string }) => void) | null;
  onerror: ((ev: ErrorEvent | Error) => void) | null;
  onmessage: ((ev: { data: string | ArrayBuffer | Blob }) => void) | null;
}

export type WebSocketConstructor = new (url: string, protocols?: string | string[]) => WebSocketLike;

const OPEN = 1;

/**
 * Create an ACP transport backed by a WebSocket (browser or Node `ws`).
 *
 * For Node, use {@link createNodeWebSocketTransport} or pass a `createWs`
 * factory that constructs a `ws` instance with custom headers.
 * For browsers, omit `createWs` and the global `WebSocket` is used.
 */
export function createWebSocketTransport(
  url: string,
  options: {
    createWs?: (url: string) => WebSocketLike;
  } = {}
): AcpTransport {
  const ws = options.createWs?.(url) ?? new (getGlobalWebSocket())(url);
  let queue: string[] = [];
  let open = false;

  const transport: AcpTransport = {
    send(message) {
      if (open && ws.readyState === OPEN) {
        ws.send(message);
      } else {
        queue.push(message);
      }
    },
    close() {
      ws.close();
    },
  };

  ws.onopen = () => {
    open = true;
    for (const m of queue) ws.send(m);
    queue = [];
    transport.onOpen?.();
  };

  ws.onclose = (ev) => {
    open = false;
    const code = typeof ev === "object" && "code" in ev ? (ev as { code: number }).code : 1006;
    const reason = typeof ev === "object" && "reason" in ev ? String((ev as { reason: string }).reason) : "";
    transport.onClose?.(code, reason);
  };

  ws.onerror = (ev) => {
    const err = ev instanceof Error ? ev : new Error(String(ev) || "WebSocket error");
    transport.onError?.(err);
  };

  ws.onmessage = (ev) => {
    const data = ev.data;
    if (typeof data === "string") {
      transport.onMessage?.(data);
    } else if (data instanceof ArrayBuffer) {
      transport.onMessage?.(new TextDecoder().decode(data));
    } else if (typeof Blob !== "undefined" && data instanceof Blob) {
      void data.text().then((t) => transport.onMessage?.(t));
    }
  };

  return transport;
}

function getGlobalWebSocket(): WebSocketConstructor {
  if (typeof globalThis !== "undefined" && (globalThis as { WebSocket?: WebSocketConstructor }).WebSocket) {
    return (globalThis as { WebSocket: WebSocketConstructor }).WebSocket;
  }
  throw new Error("WebSocket not available in this environment");
}

/**
 * Node-only transport using the `ws` package with custom headers.
 */
export async function createNodeWebSocketTransport(
  url: string,
  headers: Record<string, string>
): Promise<AcpTransport> {
  const { default: WebSocket } = await import("ws");
  return createWebSocketTransport(url, {
    createWs: (u) => new WebSocket(u, { headers }) as unknown as WebSocketLike,
  });
}
