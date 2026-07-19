import type { AcpTransport } from "./client.js";
import spawner from "../spawner.js";

export interface StdioTransportOptions {
  command: string;
  args?: string[];
  cwd?: string;
  env?: Record<string, string>;
}

/**
 * Create an ACP transport backed by a child-process stdio pipe.
 * Each JSON-RPC frame is sent as a single line on stdin; stdout is read
 * line-by-line and dispatched as ACP messages.
 */
export function createStdioTransport(options: StdioTransportOptions): AcpTransport {
  const proc = spawner.spawn(options.command, options.args ?? [], {
    cwd: options.cwd ?? process.cwd(),
    env: { ...process.env, ...(options.env ?? {}) },
    stdio: ["pipe", "pipe", "pipe"],
  });

  let open = false;
  let closed = false;
  const queue: string[] = [];

  function flush(): void {
    if (!proc.stdin?.writable) return;
    for (const m of queue) proc.stdin.write(m + "\n");
    queue.length = 0;
  }

  function sendOne(message: string): void {
    if (open && !closed && proc.stdin?.writable) {
      proc.stdin.write(message + "\n");
    } else {
      queue.push(message);
    }
  }

  const transport: AcpTransport = {
    send(message) {
      sendOne(message);
      if (open) flush();
    },
    close() {
      if (closed) return;
      closed = true;
      proc.stdin?.end();
      if (!proc.killed && proc.exitCode === null) {
        proc.kill("SIGTERM");
        setTimeout(() => {
          if (!proc.killed && proc.exitCode === null) {
            proc.kill("SIGKILL");
          }
        }, 5000).unref?.();
      }
    },
  };

  // Buffer stdout into line-delimited JSON-RPC frames. Non-JSON lines are
  // ignored; they are typically human-readable diagnostics on stderr.
  let buffer = "";
  proc.stdout?.on("data", (data: Buffer) => {
    buffer += data.toString("utf8");
    let idx;
    while ((idx = buffer.indexOf("\n")) !== -1) {
      const line = buffer.slice(0, idx).trim();
      buffer = buffer.slice(idx + 1);
      if (line) transport.onMessage?.(line);
    }
  });

  proc.stderr?.on("data", (data: Buffer) => {
    // ACP diagnostics/logs are emitted on stderr. In headless mode the connector
    // intentionally does not surface them to the caller to keep output clean.
  });

  proc.on("error", (err) => {
    open = false;
    transport.onError?.(err);
  });

  proc.on("exit", (code) => {
    open = false;
    transport.onClose?.(code ?? 0, "");
  });

  // Defer the open signal so the caller (AcpClient) has time to attach
  // its onMessage/onOpen handlers in the same tick.
  setImmediate(() => {
    if (closed) return;
    open = true;
    flush();
    transport.onOpen?.();
  });

  return transport;
}
