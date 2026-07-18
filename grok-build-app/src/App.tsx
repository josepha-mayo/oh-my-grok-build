import { useCallback, useEffect, useRef, useState } from "react";
import { AcpClient } from "./acp.js";
import { QrScanner } from "./QrScanner.js";
import type { AcpUpdate } from "./types.js";

type Message = {
  id: string;
  role: "user" | "agent" | "status";
  text: string;
  done?: boolean;
};

const DEFAULT_LOOP_COUNT = 3;
const MAX_LOOP_COUNT = 20;
const SLASH_COMMANDS = [
  "/help",
  "/model <model-id>",
  "/effort low|medium|high|max",
  "/yolo",
  "/loop [count] <prompt>",
  "/schedule [list|start|stop-daemon|stop <name>|run <name>|delete <name>]",
  "/btw <note>",
  "/new",
  "/clear",
  "/quit",
];

function generateId(): string {
  return `${Date.now()}-${Math.random().toString(36).slice(2, 9)}`;
}

type ParsedServerUrl = { baseUrl: string; secret: string } | null;

function parseServerUrl(raw: string): ParsedServerUrl {
  try {
    const url = new URL(raw);
    if (url.protocol !== "ws:" && url.protocol !== "wss:") {
      return null;
    }
    if (url.username || url.password) {
      return null;
    }
    const secret = url.searchParams.get("server-key");
    if (!secret) {
      return null;
    }
    url.searchParams.delete("server-key");
    const baseUrl = url.toString();
    return { baseUrl, secret };
  } catch {
    return null;
  }
}

function buildServerUrl(baseUrl: string, secret: string): string {
  const separator = baseUrl.includes("?") ? "&" : "?";
  return `${baseUrl}${separator}server-key=${encodeURIComponent(secret)}`;
}

function safeGet(key: string, storage: Storage): string | null {
  try {
    return storage.getItem(key);
  } catch {
    return null;
  }
}

function safeSet(key: string, value: string, storage: Storage): void {
  try {
    storage.setItem(key, value);
  } catch {
    // ignore
  }
}

function safeRemove(key: string, storage: Storage): void {
  try {
    storage.removeItem(key);
  } catch {
    // ignore
  }
}

function getRememberServer(): boolean {
  return safeGet("omgb-remember-server", localStorage) === "true";
}

function setRememberServer(value: boolean): void {
  safeSet("omgb-remember-server", String(value), localStorage);
}

function getBaseUrlStorage(remember: boolean): Storage {
  return remember ? localStorage : sessionStorage;
}

function getStoredBaseUrl(): string {
  const remember = getRememberServer();
  return safeGet("omgb-server-base-url", getBaseUrlStorage(remember)) ?? "";
}

function setStoredBaseUrl(remember: boolean, baseUrl: string): void {
  const target = getBaseUrlStorage(remember);
  const other = remember ? sessionStorage : localStorage;
  safeSet("omgb-server-base-url", baseUrl, target);
  safeRemove("omgb-server-base-url", other);
}

function getSessionSecret(): string | null {
  return safeGet("omgb-server-secret", sessionStorage);
}

function setSessionSecret(secret: string): void {
  safeSet("omgb-server-secret", secret, sessionStorage);
}

function clearSessionSecret(): void {
  safeRemove("omgb-server-secret", sessionStorage);
}

function getStoredUrl(): string {
  const base = getStoredBaseUrl();
  const secret = getSessionSecret();
  if (base && secret) {
    return buildServerUrl(base, secret);
  }
  return base;
}

function storeServerCredentials(remember: boolean, fullUrl: string): void {
  const parsed = parseServerUrl(fullUrl);
  if (!parsed) {
    return;
  }
  setStoredBaseUrl(remember, parsed.baseUrl);
  setSessionSecret(parsed.secret);
}

export default function App() {
  const [serverUrl, setServerUrl] = useState(getStoredUrl);
  const [rememberServer, setRememberServerState] = useState(getRememberServer);
  const [yolo, setYolo] = useState(
    () => safeGet("omgb-yolo", localStorage) === "true",
  );
  const [showScanner, setShowScanner] = useState(false);
  const [status, setStatus] = useState<
    "idle" | "connecting" | "connected" | "error"
  >("idle");
  const [error, setError] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);

  const clientRef = useRef<AcpClient | null>(null);
  const sessionIdRef = useRef<string | null>(null);
  const messagesEndRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  useEffect(() => {
    safeSet("omgb-yolo", String(yolo), localStorage);
  }, [yolo]);

  useEffect(() => {
    setRememberServer(rememberServer);
    storeServerCredentials(rememberServer, serverUrl);
  }, [rememberServer, serverUrl]);

  const addMessage = useCallback((message: Omit<Message, "id">): string => {
    const id = generateId();
    setMessages((prev) => [...prev, { ...message, id }]);
    return id;
  }, []);

  const updateLastAgentMessage = useCallback(
    (text: string, done = false): void => {
      setMessages((prev) => {
        const last = [...prev];
        for (let i = last.length - 1; i >= 0; i--) {
          if (last[i].role === "agent" && !last[i].done) {
            last[i] = { ...last[i], text: last[i].text + text, done };
            return last;
          }
        }
        return [...prev, { id: generateId(), role: "agent", text, done }];
      });
    },
    [],
  );

  const handleUpdate = useCallback(
    (update: AcpUpdate): void => {
      const kind = update.sessionUpdate ?? "";
      if (
        update.content?.text &&
        kind !== "agent_thought_chunk" &&
        kind !== "tool_call" &&
        kind !== "tool_call_update"
      ) {
        updateLastAgentMessage(update.content.text, false);
      }
      if (kind === "turn_completed" || kind === "stop") {
        updateLastAgentMessage("", true);
      } else if (kind === "tool_call") {
        addMessage({ role: "status", text: update.title ?? "Using tool..." });
      } else if (kind === "tool_call_update") {
        addMessage({
          role: "status",
          text: update.title ? `${update.title} update` : "Tool update",
        });
      }
    },
    [addMessage, updateLastAgentMessage],
  );

  const disconnect = useCallback(() => {
    clientRef.current?.close();
    clientRef.current = null;
    sessionIdRef.current = null;
    setStatus("idle");
  }, []);

  const connect = useCallback(
    async (urlOverride?: string) => {
      const raw = urlOverride ?? serverUrl;
      const parsed = parseServerUrl(raw);
      if (!parsed) {
        setError(
          "Invalid server URL. Make sure it is a ws:// or wss:// URL and includes ?server-key=...",
        );
        setStatus("error");
        return;
      }
      const fullUrl = buildServerUrl(parsed.baseUrl, parsed.secret);

      setError(null);
      setStatus("connecting");
      setBusy(true);
      try {
        clientRef.current?.close();
        const client = new AcpClient({
          url: fullUrl,
          yolo,
          handlers: {
            onUpdate: (_sessionId, update) => handleUpdate(update),
            onError: (err) => {
              setError(err.message);
              setStatus("error");
            },
            onClose: () => {
              setStatus("idle");
              setError("Connection closed");
            },
            onAskUser: (question) =>
              Promise.resolve(window.prompt(question) ?? ""),
          },
        });
        await client.ready();
        const init = await client.initialize(
          { terminal: true, fs: { readTextFile: true, writeTextFile: true } },
          30_000,
        );
        const authMethod = init.authMethods?.find(
          (m) => m.id === "xai.api_key",
        );
        if (!authMethod) {
          throw new Error("Server does not support xai.api_key authentication");
        }
        await client.authenticate(authMethod.id);
        const session = await client.newSession(".");
        sessionIdRef.current = session.sessionId;
        await client.setMode(session.sessionId, yolo ? "code" : "ask");
        clientRef.current = client;
        setStatus("connected");
        setServerUrl(fullUrl);
        storeServerCredentials(rememberServer, fullUrl);
        setMessages([]);
        addMessage({ role: "status", text: "Connected to Grok Build server." });
      } catch (err) {
        setStatus("error");
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setBusy(false);
      }
    },
    [serverUrl, rememberServer, yolo, addMessage, handleUpdate],
  );

  const handleQrScan = useCallback(
    (url: string) => {
      const parsed = parseServerUrl(url);
      if (!parsed) {
        setError(
          "Invalid QR code. The URL must be ws:// or wss:// and include ?server-key=...",
        );
        setStatus("error");
        setShowScanner(false);
        return;
      }
      setShowScanner(false);
      const fullUrl = buildServerUrl(parsed.baseUrl, parsed.secret);
      setServerUrl(fullUrl);
      storeServerCredentials(rememberServer, fullUrl);
      void connect(fullUrl);
    },
    [connect, rememberServer],
  );

  const doPrompt = useCallback(
    async (text: string, displayText?: string) => {
      const client = clientRef.current;
      const sessionId = sessionIdRef.current;
      if (!client || !sessionId || status !== "connected") return;
      addMessage({ role: "user", text: displayText ?? text });
      addMessage({ role: "agent", text: "" });
      setBusy(true);
      try {
        await client.prompt(sessionId, [{ type: "text", text }]);
      } catch (err) {
        addMessage({
          role: "status",
          text: err instanceof Error ? err.message : String(err),
        });
      } finally {
        setBusy(false);
      }
    },
    [addMessage, status],
  );

  const startNewSession = useCallback(async () => {
    const client = clientRef.current;
    if (!client || status !== "connected") return;
    setBusy(true);
    try {
      const session = await client.newSession(".");
      sessionIdRef.current = session.sessionId;
      await client.setMode(session.sessionId, yolo ? "code" : "ask");
      addMessage({ role: "status", text: `New session: ${session.sessionId}` });
    } catch (err) {
      addMessage({
        role: "status",
        text: err instanceof Error ? err.message : String(err),
      });
    } finally {
      setBusy(false);
    }
  }, [addMessage, status, yolo]);

  const displayCommandResult = useCallback(
    (output: string, exitCode: number) => {
      const text =
        exitCode === 0
          ? output.trim() || "(done)"
          : `[exit ${exitCode}] ${output.trim() || "(no output)"}`;
      addMessage({ role: "status", text });
    },
    [addMessage],
  );

  const doLoop = useCallback(
    async (rest: string) => {
      const client = clientRef.current;
      const sessionId = sessionIdRef.current;
      if (!client || !sessionId || status !== "connected") return;

      let count = DEFAULT_LOOP_COUNT;
      let promptText = rest;
      const match = rest.match(/^(\d+)(?:\s+(.*))?$/s);
      if (match) {
        count = Math.max(1, Math.min(MAX_LOOP_COUNT, parseInt(match[1], 10)));
        promptText = match[2]?.trim() ?? "";
      }
      if (!promptText) {
        addMessage({
          role: "status",
          text: "Usage: /loop [count] <prompt>",
        });
        return;
      }

      const args = ["loop", "--max-iterations", String(count)];
      if (yolo) args.push("--yolo");
      args.push(promptText);

      setBusy(true);
      addMessage({ role: "user", text: `/loop ${count} ${promptText}` });
      try {
        const result = await client.runTerminalCmd(args);
        displayCommandResult(result.output, result.exitCode);
      } catch (err) {
        addMessage({
          role: "status",
          text: err instanceof Error ? err.message : String(err),
        });
      } finally {
        setBusy(false);
      }
    },
    [addMessage, displayCommandResult, status, yolo],
  );

  const doBtw = useCallback(
    async (rest: string) => {
      const note = rest.trim();
      const prompt = note
        ? `[Side note / aside] ${note}\n\nThis is an off-topic aside. Do not run commands or edit files. Just reply briefly and helpfully.`
        : "[Side note / aside] What's on your mind? This is an off-topic chat; do not run commands or edit files, just reply briefly.";
      await doPrompt(prompt, note ? `/btw ${note}` : "/btw");
    },
    [doPrompt],
  );

  const doSchedule = useCallback(
    async (rest: string) => {
      const client = clientRef.current;
      if (!client || status !== "connected") return;

      const parts = rest.trim().split(/\s+/);
      const sub = parts[0]?.toLowerCase() ?? "";
      const name = parts[1];
      const args: string[] = ["schedule"];

      switch (sub) {
        case "":
        case "list":
          args.push("list");
          break;
        case "start":
          args.push("start");
          break;
        case "stop-daemon":
          args.push("stop-daemon");
          break;
        case "stop":
          if (!name) {
            addMessage({
              role: "status",
              text: "Usage: /schedule stop <name>",
            });
            return;
          }
          args.push("stop", name);
          break;
        case "run":
          if (!name) {
            addMessage({ role: "status", text: "Usage: /schedule run <name>" });
            return;
          }
          args.push("run", name);
          break;
        case "delete":
          if (!name) {
            addMessage({
              role: "status",
              text: "Usage: /schedule delete <name>",
            });
            return;
          }
          args.push("delete", name);
          break;
        default:
          addMessage({
            role: "status",
            text: "Usage: /schedule [list|start|stop-daemon|stop <name>|run <name>|delete <name>]",
          });
          return;
      }

      setBusy(true);
      addMessage({ role: "user", text: `/schedule ${rest}` });
      try {
        const result = await client.runTerminalCmd(args);
        displayCommandResult(result.output, result.exitCode);
      } catch (err) {
        addMessage({
          role: "status",
          text: err instanceof Error ? err.message : String(err),
        });
      } finally {
        setBusy(false);
      }
    },
    [addMessage, displayCommandResult, status],
  );

  const handleSlash = useCallback(
    async (text: string) => {
      const parts = text.split(/\s+/);
      const command = parts[0];
      const rest = parts.slice(1).join(" ");

      switch (command) {
        case "/help":
          addMessage({
            role: "status",
            text: `Commands: ${SLASH_COMMANDS.join(", ")}`,
          });
          return;
        case "/btw":
          await doBtw(rest);
          return;
        case "/clear":
          setMessages([]);
          return;
        case "/quit":
          disconnect();
          return;
        case "/new":
          await startNewSession();
          return;
        case "/yolo": {
          const nextYolo = !yolo;
          setYolo(nextYolo);
          const client = clientRef.current;
          const sessionId = sessionIdRef.current;
          if (client && sessionId && status === "connected") {
            setBusy(true);
            try {
              await client.setMode(sessionId, nextYolo ? "code" : "ask");
            } catch (err) {
              addMessage({
                role: "status",
                text: err instanceof Error ? err.message : String(err),
              });
            } finally {
              setBusy(false);
            }
          }
          return;
        }
        case "/loop":
          await doLoop(rest);
          return;
        case "/model": {
          const modelId = rest.trim();
          if (!modelId) {
            addMessage({ role: "status", text: "Usage: /model <model-id>" });
            return;
          }
          const client = clientRef.current;
          const sessionId = sessionIdRef.current;
          if (!client || !sessionId || status !== "connected") return;
          setBusy(true);
          try {
            await client.setModel(sessionId, modelId);
            addMessage({ role: "status", text: `Model set to ${modelId}` });
          } catch (err) {
            addMessage({
              role: "status",
              text: err instanceof Error ? err.message : String(err),
            });
          } finally {
            setBusy(false);
          }
          return;
        }
        case "/effort": {
          const effort = rest.trim();
          if (!["low", "medium", "high", "max"].includes(effort)) {
            addMessage({
              role: "status",
              text: "Usage: /effort low|medium|high|max",
            });
            return;
          }
          const client = clientRef.current;
          const sessionId = sessionIdRef.current;
          if (!client || !sessionId || status !== "connected") return;
          setBusy(true);
          try {
            const ok = await client.setEffort(sessionId, effort);
            addMessage({
              role: "status",
              text: ok
                ? `Reasoning effort set to ${effort}`
                : "Reasoning effort is not configurable on this agent.",
            });
          } catch (err) {
            addMessage({
              role: "status",
              text: err instanceof Error ? err.message : String(err),
            });
          } finally {
            setBusy(false);
          }
          return;
        }
        case "/schedule":
          await doSchedule(rest);
          return;
        default:
          await doPrompt(text);
      }
    },
    [
      addMessage,
      disconnect,
      doBtw,
      doLoop,
      doPrompt,
      doSchedule,
      startNewSession,
      status,
      yolo,
    ],
  );

  const send = useCallback(async () => {
    const text = input.trim();
    if (
      !text ||
      !clientRef.current ||
      !sessionIdRef.current ||
      status !== "connected" ||
      busy
    )
      return;
    setInput("");
    if (text.startsWith("/")) {
      await handleSlash(text);
    } else {
      await doPrompt(text);
    }
  }, [input, status, busy, handleSlash, doPrompt]);

  return (
    <div className="app">
      <header className="header">
        <h1>Grok Build Mobile</h1>
        <div className="status">
          {status === "connected" ? "● connected" : status}
        </div>
      </header>

      <main className="main">
        {status !== "connected" ? (
          <div className="connect-panel">
            <label>
              Server URL
              <input
                type="text"
                value={serverUrl}
                onChange={(e) => setServerUrl(e.target.value)}
                placeholder="ws://host:port/ws?server-key=..."
              />
            </label>
            <div className="actions">
              <button onClick={() => void connect()} disabled={busy}>
                Connect
              </button>
              <button onClick={() => setShowScanner((s) => !s)} disabled={busy}>
                {showScanner ? "Hide scanner" : "Scan QR"}
              </button>
            </div>
            {showScanner && (
              <div className="scanner">
                <QrScanner
                  onScan={handleQrScan}
                  onError={(err) => setError(err.message)}
                />
              </div>
            )}
            <label className="inline">
              <input
                type="checkbox"
                checked={rememberServer}
                onChange={(e) => setRememberServerState(e.target.checked)}
              />
              Remember server address on this device (secret stays in this
              session only)
            </label>
            <label className="inline">
              <input
                type="checkbox"
                checked={yolo}
                onChange={(e) => setYolo(e.target.checked)}
              />
              Auto-approve (yolo)
            </label>
            {error && <div className="error">{error}</div>}
          </div>
        ) : (
          <div className="chat">
            <div className="messages">
              {messages.map((m) => (
                <div key={m.id} className={`message ${m.role}`}>
                  <div className="bubble">
                    {m.text ||
                      (m.role === "agent" && !m.done ? (
                        <em>Thinking...</em>
                      ) : (
                        ""
                      ))}
                  </div>
                </div>
              ))}
              <div ref={messagesEndRef} />
            </div>
            <div className="composer">
              <input
                type="text"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && !busy && void send()}
                placeholder="Ask Grok Build..."
                disabled={busy}
              />
              <button
                onClick={() => void send()}
                disabled={busy || !input.trim()}
              >
                Send
              </button>
              <button onClick={disconnect} disabled={busy}>
                Disconnect
              </button>
            </div>
          </div>
        )}
      </main>
    </div>
  );
}
