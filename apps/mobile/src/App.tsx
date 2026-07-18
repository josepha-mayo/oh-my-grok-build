import { useState, useRef, useCallback, useEffect } from "react";
import { AcpClient, type AcpPermissionRequest, type AcpUpdate, type AcpPermissionResponse } from "./acp/client";
import { ConnectionScreen } from "./components/ConnectionScreen";
import { ChatScreen, type Message, SLASH_COMMANDS } from "./components/ChatScreen";
import type { ToolOutputData } from "./components/ToolOutput";
import type { ReasoningEffort } from "./components/EffortPicker";
import { notifyCompletion, requestNotificationPermission } from "./notifications";
import "./App.css";

type View = "connect" | "chat";
type ConnectionStatus = "connecting" | "connected" | "disconnected";

const CONNECTIONS_KEY = "omgb:connections";
const SERVER_KEYS_KEY = "omgb:serverKeys";

interface LoopState {
  remaining: number;
  basePrompt: string;
}

function stripSecret(url: string): string {
  try {
    const u = new URL(url);
    u.searchParams.delete("server-key");
    return u.toString();
  } catch {
    return url;
  }
}

function extractSecret(url: string): string | undefined {
  try {
    return new URL(url).searchParams.get("server-key") ?? undefined;
  } catch {
    return undefined;
  }
}

function getServerKey(safeUrl: string): string | undefined {
  try {
    const raw = sessionStorage.getItem(SERVER_KEYS_KEY);
    const map = raw ? (JSON.parse(raw) as Record<string, string>) : {};
    return map[safeUrl];
  } catch {
    return undefined;
  }
}

function setServerKey(safeUrl: string, secret: string | undefined): void {
  try {
    const raw = sessionStorage.getItem(SERVER_KEYS_KEY);
    const map = raw ? (JSON.parse(raw) as Record<string, string>) : {};
    if (secret) map[safeUrl] = secret;
    sessionStorage.setItem(SERVER_KEYS_KEY, JSON.stringify(map));
  } catch {
    // ignore
  }
}

function restoreSecret(url: string): string {
  try {
    const u = new URL(url);
    if (u.searchParams.has("server-key")) return url;
    const safeUrl = stripSecret(url);
    const secret = getServerKey(safeUrl);
    if (secret) u.searchParams.set("server-key", secret);
    return u.toString();
  } catch {
    return url;
  }
}

function loadLastUrl(): string {
  try {
    const raw = localStorage.getItem(CONNECTIONS_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : [];
    if (Array.isArray(parsed) && parsed.length > 0) {
      const first = parsed[0] as { url?: string };
      return first.url ?? "";
    }
  } catch {
    // ignore
  }
  return "";
}

function saveConnection(url: string) {
  const safeUrl = stripSecret(url);
  const secret = extractSecret(url) ?? getServerKey(safeUrl);
  setServerKey(safeUrl, secret);
  try {
    const raw = localStorage.getItem(CONNECTIONS_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : [];
    const list = Array.isArray(parsed)
      ? (parsed as { url?: string; name?: string }[]).filter(
          (c): c is { url: string; name?: string } => typeof c.url === "string"
        )
      : [];
    const without = list.filter((c) => c.url !== safeUrl);
    const next = [{ url: safeUrl, name: safeUrl }, ...without].slice(0, 20);
    localStorage.setItem(CONNECTIONS_KEY, JSON.stringify(next));
  } catch {
    // ignore
  }
}

function getText(content: unknown): string {
  if (typeof content === "string") return content;
  if (content && typeof content === "object") {
    const c = content as { text?: string };
    return c.text ?? "";
  }
  return "";
}

function parseToolOutput(raw: unknown): ToolOutputData | undefined {
  if (raw == null) return undefined;

  let parsed: unknown = raw;
  if (typeof raw === "string") {
    try {
      parsed = JSON.parse(raw);
    } catch {
      return { text: raw };
    }
  }

  if (typeof parsed !== "object" || parsed === null) {
    return { text: String(raw) };
  }

  const o = parsed as Record<string, unknown>;
  const out: ToolOutputData = {};

  if (typeof o.terminal === "string") out.terminal = o.terminal;
  if (typeof o.text === "string") out.text = o.text;
  if ("diff" in o) out.diff = o.diff as ToolOutputData["diff"];
  if (typeof o.image === "string") out.image = o.image;
  if (typeof o.screenshot === "string") out.screenshot = o.screenshot;

  if (!out.terminal && !out.diff && !out.image && !out.screenshot && !out.text) {
    out.text = JSON.stringify(o, null, 2);
  }

  return out;
}

export default function App() {
  const [view, setView] = useState<View>("connect");
  const [url, setUrl] = useState(loadLastUrl);
  const [model, setModel] = useState("grok-build");
  const [effort, setEffort] = useState<ReasoningEffort>("medium");
  const [yolo, setYolo] = useState(false);
  const [messages, setMessages] = useState<Message[]>([]);
  const [thinking, setThinking] = useState(false);
  const [permission, setPermission] = useState<AcpPermissionRequest | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [connectionStatus, setConnectionStatus] = useState<ConnectionStatus>("disconnected");
  const [availableModels, setAvailableModels] = useState<string[]>([]);

  const clientRef = useRef<AcpClient | null>(null);
  const permissionResolver = useRef<((value: AcpPermissionResponse) => void) | null>(null);
  const closingRef = useRef(false);
  const loopRef = useRef<LoopState | null>(null);
  const sessionIdRef = useRef<string>("");

  useEffect(() => {
    closingRef.current = false;
  }, []);

  const appendMessage = useCallback((role: Message["role"], update?: Partial<Message>) => {
    setMessages((prev) => {
      const last = prev[prev.length - 1];
      if (last && last.role === role && !update?.tool && !last.tool) {
        const next = [...prev];
        next[next.length - 1] = { ...last, ...update, text: last.text + (update?.text ?? "") };
        return next;
      }
      return [...prev, { id: `${Date.now()}-${Math.random()}`, role, text: update?.text ?? "", ...update }];
    });
  }, []);

  const maybeContinueLoop = useCallback(async () => {
    const client = clientRef.current;
    const loop = loopRef.current;
    if (!client || !loop || loop.remaining <= 0 || !sessionIdRef.current) return;

    loop.remaining -= 1;
    if (loop.remaining < 0) {
      loopRef.current = null;
      return;
    }

    const prompt =
      loop.remaining > 0
        ? `Review the result above, fix any issues, and continue. Original task: ${loop.basePrompt}`
        : `Wrap up and finalize. Original task: ${loop.basePrompt}`;

    setThinking(true);
    appendMessage("agent", { text: `[loop] ${loop.remaining} iteration${loop.remaining === 1 ? "" : "s"} remaining` });
    try {
      await client.prompt(sessionIdRef.current, [{ type: "text", text: prompt }]);
    } catch (err) {
      appendMessage("agent", { text: `Loop error: ${err instanceof Error ? err.message : String(err)}` });
      setThinking(false);
      loopRef.current = null;
    }
  }, [appendMessage]);

  const handleUpdate = useCallback(
    (_sessionId: string, update: AcpUpdate) => {
      if (closingRef.current || _sessionId !== sessionIdRef.current) return;
      switch (update.sessionUpdate) {
        case "agent_message_chunk": {
          appendMessage("agent", { text: getText(update.content) });
          break;
        }
        case "agent_thought_chunk": {
          appendMessage("thought", { text: getText(update.content) });
          break;
        }
        case "tool_call": {
          appendMessage("agent", { tool: { title: update.title ?? "tool", status: "running" }, text: "" });
          break;
        }
        case "tool_call_update": {
          const output = parseToolOutput(update.output ?? update.content);
          setMessages((prev) => {
            const toolIndex = [...prev].reverse().findIndex((m) => m.tool);
            if (toolIndex === -1) return prev;
            const actualIndex = prev.length - 1 - toolIndex;
            const next = [...prev];
            next[actualIndex] = {
              ...next[actualIndex],
              tool: {
                ...next[actualIndex].tool,
                status: update.status ?? next[actualIndex].tool?.status ?? "running",
                ...(output ? { output } : {}),
              },
            };
            return next;
          });
          break;
        }
        case "turn_completed":
        case "stop": {
          setThinking(false);
          void notifyCompletion("Grok completed");
          if (loopRef.current && loopRef.current.remaining > 0) {
            void maybeContinueLoop();
          } else {
            loopRef.current = null;
          }
          break;
        }
      }
    },
    [appendMessage, maybeContinueLoop]
  );

  const handlePermission = useCallback((req: AcpPermissionRequest): Promise<AcpPermissionResponse> => {
    setPermission(req);
    return new Promise((resolve) => {
      permissionResolver.current = resolve;
    });
  }, []);

  const handlePermissionSelect = useCallback((optionId: string) => {
    setPermission(null);
    permissionResolver.current?.({ outcome: { outcome: "selected", optionId } });
    permissionResolver.current = null;
  }, []);

  const handleModelsUpdate = useCallback((models: string[]) => {
    setAvailableModels((prev) => Array.from(new Set([...prev, ...models])));
  }, []);

  const startSessionWithProfile = useCallback(
    async (client: AcpClient, modelId: string, yoloMode: boolean, reasoningEffort: ReasoningEffort) => {
      const { sessionId: sid } = await client.newSession(".", [], { modelId, yoloMode, reasoningEffort }, 60_000);
      sessionIdRef.current = sid;
      setThinking(false);
      loopRef.current = null;
      return sid;
    },
    []
  );

  const matchSlashCommand = useCallback((text: string): { cmd: string; arg: string } | null => {
    const trimmed = text.trim();
    for (const c of SLASH_COMMANDS) {
      if (trimmed === c.id || trimmed.startsWith(c.id + " ")) {
        return { cmd: c.id, arg: trimmed.slice(c.id.length).trim() };
      }
    }
    return null;
  }, []);

  const onSend = useCallback(
    async (text: string) => {
      const client = clientRef.current;
      const currentSessionId = sessionIdRef.current;
      if (!client || !currentSessionId) return;

      const slash = matchSlashCommand(text);
      if (slash) {
        const { cmd, arg } = slash;
        switch (cmd) {
          case "/clear":
            setMessages([]);
            return;
          case "/new":
            appendMessage("agent", { text: "Use disconnect to start a new session." });
            return;
          case "/yolo":
          case "/autonomous":
          case "/devin autonomous": {
            const next = !yolo;
            setYolo(next);
            appendMessage("agent", { text: `Auto-approve ${next ? "enabled" : "disabled"}.` });
            try {
              await startSessionWithProfile(client, model, next, effort);
              appendMessage("agent", { text: `Session restarted with auto-approve ${next ? "on" : "off"}.` });
            } catch (err) {
              appendMessage("agent", {
                text: `Failed to switch mode: ${err instanceof Error ? err.message : String(err)}`,
              });
            }
            return;
          }
          case "/model":
            if (arg) {
              try {
                await client.setModel(currentSessionId, arg);
                setModel(arg);
                appendMessage("agent", { text: `Model set to ${arg}.` });
              } catch (err) {
                appendMessage("agent", {
                  text: `Failed to set model: ${err instanceof Error ? err.message : String(err)}`,
                });
              }
            }
            return;
          case "/effort":
            if (arg && ["low", "medium", "high", "max"].includes(arg)) {
              const e = arg as ReasoningEffort;
              setEffort(e);
              appendMessage("agent", { text: `Reasoning effort set to ${e}.` });
              try {
                await startSessionWithProfile(client, model, yolo, e);
                appendMessage("agent", { text: `Session restarted with effort ${e}.` });
              } catch (err) {
                appendMessage("agent", {
                  text: `Failed to set effort: ${err instanceof Error ? err.message : String(err)}`,
                });
              }
            } else {
              appendMessage("agent", { text: "Usage: /effort low|medium|high|max" });
            }
            return;
          case "/loop":
          case "/devin loop": {
            loopRef.current = { remaining: 3, basePrompt: arg };
            appendMessage("user", { text: arg });
            setThinking(true);
            try {
              await client.prompt(currentSessionId, [{ type: "text", text: arg }]);
            } catch (err) {
              appendMessage("agent", { text: `Error: ${err instanceof Error ? err.message : String(err)}` });
              setThinking(false);
              loopRef.current = null;
            }
            return;
          }
          case "/swarm": {
            const quoted = JSON.stringify(arg);
            appendMessage("agent", {
              text: `Swarm mode runs on the desktop CLI. Copy and run:\n\`\`\`\nomgb swarm ${quoted}\n\`\`\``,
            });
            return;
          }
          case "/plan":
            appendMessage("agent", { text: "Plan mode cannot be toggled from mobile yet." });
            return;
          case "/help":
            appendMessage("agent", {
              text: SLASH_COMMANDS.map((c) => `${c.id}${c.args ? ` ${c.args}` : ""}`).join("\n"),
            });
            return;
          default:
            appendMessage("agent", { text: `Unknown command: ${cmd}` });
            return;
        }
      }

      // Normal user message cancels an active loop so we do not unexpectedly keep iterating.
      loopRef.current = null;
      appendMessage("user", { text });
      setThinking(true);
      try {
        await client.prompt(currentSessionId, [{ type: "text", text }]);
      } catch (err) {
        appendMessage("agent", { text: `Error: ${err instanceof Error ? err.message : String(err)}` });
        setThinking(false);
      }
    },
    [appendMessage, yolo, model, effort, startSessionWithProfile]
  );

  const handleModelChange = useCallback(
    async (modelId: string) => {
      const client = clientRef.current;
      if (client && sessionIdRef.current) {
        try {
          await client.setModel(sessionIdRef.current, modelId);
          setModel(modelId);
          appendMessage("agent", { text: `Model set to ${modelId}.` });
        } catch (err) {
          appendMessage("agent", {
            text: `Failed to set model: ${err instanceof Error ? err.message : String(err)}`,
          });
        }
      } else {
        setModel(modelId);
        appendMessage("agent", { text: `Model preference set to ${modelId}.` });
      }
    },
    [appendMessage]
  );

  const handleEffortChange = useCallback(
    async (e: ReasoningEffort) => {
      const client = clientRef.current;
      setEffort(e);
      if (client && sessionIdRef.current) {
        try {
          await startSessionWithProfile(client, model, yolo, e);
          appendMessage("agent", { text: `Reasoning effort set to ${e}.` });
        } catch (err) {
          appendMessage("agent", {
            text: `Failed to set effort: ${err instanceof Error ? err.message : String(err)}`,
          });
        }
      } else {
        appendMessage("agent", { text: `Effort preference set to ${e}.` });
      }
    },
    [appendMessage, model, yolo, startSessionWithProfile]
  );

  const connect = useCallback(
    async (connectUrl: string, switchView = true, clearHistory = true) => {
      setError(null);
      closingRef.current = false;
      clientRef.current?.close();
      clientRef.current = null;
      const safeUrl = stripSecret(connectUrl);
      const fullUrl = restoreSecret(connectUrl);
      setUrl(safeUrl);
      if (clearHistory) setMessages([]);
      setPermission(null);
      setConnectionStatus("connecting");

      const client = new AcpClient(fullUrl, {
        onOpen: () => setConnectionStatus("connected"),
        onClose: () => {
          setConnectionStatus("disconnected");
          if (!closingRef.current) setError("Connection closed");
        },
        onError: (err) => {
          setConnectionStatus("disconnected");
          setError(err.message);
        },
        onUpdate: handleUpdate,
        onPermission: handlePermission,
        onModelsUpdate: handleModelsUpdate,
      });
      clientRef.current = client;

      try {
        const init = await client.initialize(
          1,
          { terminal: true, fs: { readTextFile: true, writeTextFile: true } },
          30_000
        );
        const authMethod = init.authMethods?.find((m) => m.id === "xai.api_key") ?? init.authMethods?.[0];
        if (authMethod) {
          await client.authenticate(authMethod, 60_000);
        }
        await startSessionWithProfile(client, model, yolo, effort);
        setConnectionStatus("connected");
        if (switchView) setView("chat");
        saveConnection(connectUrl);

        void requestNotificationPermission();
      } catch (err) {
        closingRef.current = true;
        client.close();
        setConnectionStatus("disconnected");
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [handleUpdate, handlePermission, handleModelsUpdate, model, yolo, effort, startSessionWithProfile]
  );

  const onConnect = useCallback(
    (connectUrl: string) => {
      void connect(connectUrl, true, true);
    },
    [connect]
  );

  const onDisconnect = useCallback(() => {
    closingRef.current = true;
    clientRef.current?.close();
    clientRef.current = null;
    loopRef.current = null;
    sessionIdRef.current = "";
    setView("connect");
    setMessages([]);
    setPermission(null);
    setError(null);
    setThinking(false);
    setConnectionStatus("disconnected");
  }, []);

  const onReconnect = useCallback(() => {
    void connect(url, false, false);
  }, [connect, url]);

  const onQuickSync = useCallback(() => {
    if (!url) return;
    setMessages((prev) => {
      if (prev.length > 50) {
        return prev.slice(-50);
      }
      return prev;
    });
    void connect(url, false, false);
  }, [connect, url]);

  if (view === "connect") {
    return (
      <div className="app">
        <ConnectionScreen onConnect={onConnect} defaultUrl={url} />
        {error ? <div className="toast error">{error}</div> : null}
      </div>
    );
  }

  return (
    <div className="app">
      <ChatScreen
        url={url}
        model={model}
        effort={effort}
        yolo={yolo}
        messages={messages}
        thinking={thinking}
        permission={permission}
        connectionStatus={connectionStatus}
        availableModels={availableModels}
        onSend={onSend}
        onPermissionSelect={handlePermissionSelect}
        onDisconnect={onDisconnect}
        onReconnect={onReconnect}
        onQuickSync={onQuickSync}
        onModelChange={handleModelChange}
        onEffortChange={handleEffortChange}
        onConnectSaved={onConnect}
        onClear={() => setMessages([])}
      />
      {error ? <div className="toast error">{error}</div> : null}
    </div>
  );
}
