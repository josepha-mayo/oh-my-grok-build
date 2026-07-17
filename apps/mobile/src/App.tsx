import { useState, useRef, useCallback } from "react";
import { AcpClient, type AcpPermissionRequest, type AcpUpdate, type AcpPermissionResponse } from "./acp/client";
import { ConnectionScreen } from "./components/ConnectionScreen";
import { ChatScreen, type Message } from "./components/ChatScreen";
import "./App.css";

type View = "connect" | "chat";

export default function App() {
  const [view, setView] = useState<View>("connect");
  const [url, setUrl] = useState("");
  const [sessionId, setSessionId] = useState("");
  const [model, setModel] = useState("grok-build");
  const [yolo, setYolo] = useState(false);
  const [messages, setMessages] = useState<Message[]>([]);
  const [thinking, setThinking] = useState(false);
  const [permission, setPermission] = useState<AcpPermissionRequest | null>(null);
  const [error, setError] = useState<string | null>(null);

  const clientRef = useRef<AcpClient | null>(null);
  const permissionResolver = useRef<((value: AcpPermissionResponse) => void) | null>(null);

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
          const text = (update.content as { text?: string } | undefined)?.text ?? "";
          appendMessage("agent", { text });
          break;
        }
        case "tool_call":
          appendMessage("agent", { tool: { title: update.title ?? "tool", status: "running" }, text: "" });
          break;
        case "tool_call_update":
          setMessages((prev) => {
            const next = [...prev];
            const lastTool = [...next].reverse().find((m) => m.tool);
            if (lastTool?.tool) lastTool.tool.status = update.status ?? lastTool.tool.status;
            return next;
          });
          break;
        case "turn_completed":
        case "stop":
          setThinking(false);
          break;
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

  const onConnect = useCallback(
    async (connectUrl: string) => {
      setError(null);
      setUrl(connectUrl);
      setMessages([]);

      const client = new AcpClient(connectUrl, {
        onOpen: () => {},
        onClose: () => setError("Connection closed"),
        onError: (err) => setError(err.message),
        onUpdate: handleUpdate,
        onPermission: handlePermission,
      });

      clientRef.current = client;

      try {
        await client.initialize(1, { terminal: true, fs_read: true, fs_write: false });
        const { sessionId: sid } = await client.newSession("/", [], { yoloMode: yolo });
        setSessionId(sid);
        setView("chat");
      } catch (err) {
        client.close();
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [handleUpdate, handlePermission, yolo]
  );

  const onDisconnect = useCallback(() => {
    clientRef.current?.close();
    clientRef.current = null;
    setView("connect");
    setSessionId("");
    setMessages([]);
    setPermission(null);
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
        onSend={onSend}
        onPermissionSelect={handlePermissionSelect}
        onDisconnect={onDisconnect}
        onModelChange={setModel}
        onYoloToggle={() => setYolo((v) => !v)}
        onClear={() => setMessages([])}
      />
      {error ? <div className="toast error">{error}</div> : null}
    </div>
  );
}
