import { useState, useRef, useCallback, useEffect } from "react";
import { AcpClient, type AcpPermissionRequest, type AcpUpdate, type AcpPermissionResponse } from "./acp/client";
import { ConnectionScreen } from "./components/ConnectionScreen";
import { ChatScreen, type Message } from "./components/ChatScreen";
import type { ToolOutputData } from "./components/ToolOutput";
import "./App.css";

type View = "connect" | "chat";
type ConnectionStatus = "connecting" | "connected" | "disconnected";

const CONNECTIONS_KEY = "omgb:connections";

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
  try {
    const raw = localStorage.getItem(CONNECTIONS_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : [];
    const list = Array.isArray(parsed)
      ? (parsed as { url?: string; name?: string }[]).filter((c): c is { url: string; name?: string } => typeof c.url === "string")
      : [];
    const without = list.filter((c) => c.url !== url);
    const next = [{ url, name: url }, ...without].slice(0, 20);
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
  const [sessionId, setSessionId] = useState("");
  const [model, setModel] = useState("grok-build");
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

  useEffect(() => {
    // Reset close flag on mount so disconnect-to-reconnect cycles behave.
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

  const handleUpdate = useCallback(
    (_sessionId: string, update: AcpUpdate) => {
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
            const next = [...prev];
            const lastTool = [...next].reverse().find((m) => m.tool);
            if (lastTool?.tool) {
              lastTool.tool.status = update.status ?? lastTool.tool.status;
              if (output) lastTool.tool.output = output;
            }
            return next;
          });
          break;
        }
        case "turn_completed":
        case "stop": {
          setThinking(false);
          break;
        }
      }
    },
    [appendMessage]
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

  const onSend = useCallback(
    async (text: string) => {
      const client = clientRef.current;
      if (!client || !sessionId) return;

      if (text.startsWith("/")) {
        const [cmd, ...rest] = text.trim().split(/\s+/);
        const arg = rest.join(" ").trim();
        switch (cmd) {
          case "/clear":
            setMessages([]);
            return;
          case "/new":
            appendMessage("agent", { text: "Use disconnect to start a new session." });
            return;
          case "/yolo":
            setYolo((v) => {
              const next = !v;
              appendMessage("agent", { text: `Auto-approve ${next ? "enabled" : "disabled"}.` });
              return next;
            });
            return;
          case "/model":
            if (arg) {
              setModel(arg);
              appendMessage("agent", { text: `Model preference set to ${arg}.` });
            }
            return;
          case "/loop":
            appendMessage("agent", { text: "Scheduling is not yet implemented in mobile." });
            return;
          case "/plan":
            appendMessage("agent", { text: "Plan mode cannot be toggled from mobile yet." });
            return;
          case "/help":
            appendMessage("agent", {
              text: ["/model <id>", "/yolo", "/clear", "/new", "/loop <interval> <prompt>", "/plan", "/help"].join("\n"),
            });
            return;
          default:
            appendMessage("agent", { text: `Unknown command: ${cmd}` });
            return;
        }
      }

      appendMessage("user", { text });
      setThinking(true);
      try {
        await client.prompt(sessionId, [{ type: "text", text }]);
      } catch (err) {
        appendMessage("agent", { text: `Error: ${err instanceof Error ? err.message : String(err)}` });
        setThinking(false);
      }
    },
    [sessionId, appendMessage]
  );

  const handleModelChange = useCallback(
    (modelId: string) => {
      setModel(modelId);
      appendMessage("agent", { text: `Model preference set to ${modelId}.` });
    },
    [appendMessage]
  );

  const connect = useCallback(
    async (connectUrl: string, switchView = true, clearHistory = true) => {
      setError(null);
      closingRef.current = false;
      clientRef.current?.close();
      clientRef.current = null;
      setUrl(connectUrl);
      if (clearHistory) setMessages([]);
      setPermission(null);
      setConnectionStatus("connecting");

      const client = new AcpClient(connectUrl, {
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
        await client.initialize(1, { terminal: true, fs: { readTextFile: true, writeTextFile: false } }, 30_000);
        const { sessionId: sid } = await client.newSession("/", [], { yoloMode: yolo, modelId: model }, 60_000);
        setSessionId(sid);
        setConnectionStatus("connected");
        if (switchView) setView("chat");
        saveConnection(connectUrl);
      } catch (err) {
        closingRef.current = true;
        client.close();
        setConnectionStatus("disconnected");
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [handleUpdate, handlePermission, handleModelsUpdate, yolo, model]
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
    setView("connect");
    setSessionId("");
    setMessages([]);
    setPermission(null);
    setError(null);
    setThinking(false);
    setConnectionStatus("disconnected");
  }, []);

  const onReconnect = useCallback(() => {
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
        onModelChange={handleModelChange}
        onConnectSaved={onConnect}
        onYoloToggle={() => setYolo((v) => !v)}
        onClear={() => setMessages([])}
      />
      {error ? <div className="toast error">{error}</div> : null}
    </div>
  );
}
