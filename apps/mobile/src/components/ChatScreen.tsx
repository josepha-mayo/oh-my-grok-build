import { useRef, useEffect, useState } from "react";
import { ArrowLeft, Send, Command, Bot, User, Settings as SettingsIcon, Trash2, RefreshCw } from "lucide-react";
import type { AcpPermissionRequest } from "../acp/client";
import { PermissionCard } from "./PermissionCard";
import { ToolOutput, type ToolOutputData } from "./ToolOutput";
import { ModelPicker } from "./ModelPicker";
import { Settings } from "./Settings";

export interface Message {
  id: string;
  role: "user" | "agent" | "thought";
  text: string;
  tool?: { title?: string; status?: string; output?: ToolOutputData };
}

interface ChatScreenProps {
  url: string;
  model: string;
  yolo: boolean;
  messages: Message[];
  thinking: boolean;
  permission: AcpPermissionRequest | null;
  connectionStatus: "connecting" | "connected" | "disconnected";
  availableModels: string[];
  onSend: (text: string) => void;
  onPermissionSelect: (optionId: string) => void;
  onDisconnect: () => void;
  onReconnect: () => void;
  onModelChange: (model: string) => void;
  onConnectSaved: (url: string) => void;
  onYoloToggle: () => void;
  onClear: () => void;
}

const SLASH_COMMANDS = [
  { id: "/model", label: "Switch model", args: "<model-id>" },
  { id: "/loop", label: "Run on a schedule", args: "<interval> <prompt>" },
  { id: "/plan", label: "Enter plan mode" },
  { id: "/yolo", label: "Toggle auto-approve" },
  { id: "/clear", label: "Clear conversation" },
  { id: "/new", label: "New session" },
  { id: "/help", label: "Show commands" },
];

export function ChatScreen({
  url,
  model,
  yolo,
  messages,
  thinking,
  permission,
  connectionStatus,
  availableModels,
  onSend,
  onPermissionSelect,
  onDisconnect,
  onReconnect,
  onModelChange,
  onConnectSaved,
  onClear,
}: ChatScreenProps) {
  const [input, setInput] = useState("");
  const [slashQuery, setSlashQuery] = useState("");
  const [showModelPicker, setShowModelPicker] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: scrollRef.current.scrollHeight, behavior: "smooth" });
  }, [messages]);

  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

  useEffect(() => {
    const ta = textareaRef.current;
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = `${Math.min(ta.scrollHeight, 140)}px`;
  }, [input]);

  const submit = (text: string) => {
    if (!text.trim() || thinking) return;
    setInput("");
    setSlashQuery("");
    onSend(text);
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit(input);
    }
  };

  const onInputChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const value = e.target.value;
    setInput(value);
    if (value.startsWith("/")) {
      const space = value.indexOf(" ");
      setSlashQuery(space > 0 ? value.slice(1, space) : value.slice(1));
    } else {
      setSlashQuery("");
    }
  };

  const filteredCommands = slashQuery
    ? SLASH_COMMANDS.filter(
        (c) => c.id.startsWith("/" + slashQuery) || c.label.toLowerCase().includes(slashQuery.toLowerCase())
      )
    : [];

  const insertCommand = (id: string) => {
    setInput(`${id} `);
    textareaRef.current?.focus();
  };

  const handleModelSelect = (m: string) => {
    onModelChange(m);
  };

  return (
    <div className="chat-screen safe-area">
      <header className="chat-header">
        <button onClick={onDisconnect} className="icon-button">
          <ArrowLeft size={22} />
        </button>

        <button className="header-meta" onClick={() => setShowModelPicker(true)}>
          <span className={`status-dot ${connectionStatus}`} />
          <span className="model-badge">{model}</span>
          {yolo ? <span className="yolo-badge">YOLO</span> : null}
        </button>

        <div className="header-actions">
          {connectionStatus === "disconnected" ? (
            <button className="icon-button reconnect-button" onClick={onReconnect} title="Reconnect">
              <RefreshCw size={20} />
            </button>
          ) : null}
          <button className="icon-button" onClick={() => setShowSettings(true)} title="Settings">
            <SettingsIcon size={20} />
          </button>
          <button className="icon-button" onClick={onClear} title="Clear chat">
            <Trash2 size={20} />
          </button>
        </div>
      </header>

      <div className="chat-scroll" ref={scrollRef}>
        {messages.length === 0 && (
          <div className="empty-state">
            <Bot size={40} />
            <p>Connected to {url.replace(/\?.*$/, "")}</p>
            <p className="hint">Type a prompt or / command.</p>
          </div>
        )}

        {messages.map((m) => {
          if (m.role === "thought") {
            return (
              <details key={m.id} className="thinking-bubble">
                <summary>Thinking</summary>
                <pre>{m.text}</pre>
              </details>
            );
          }

          return (
            <div key={m.id} className={`message ${m.role}`}>
              <div className="message-avatar">{m.role === "user" ? <User size={16} /> : <Bot size={16} />}</div>
              <div className="message-body">
                {m.tool ? (
                  <div className="tool-card">
                    <div className="tool-pill">
                      {m.tool.title} {m.tool.status ? `· ${m.tool.status}` : ""}
                    </div>
                    {m.tool.output ? <ToolOutput output={m.tool.output} /> : null}
                  </div>
                ) : null}
                {m.text}
              </div>
            </div>
          );
        })}

        {thinking && <div className="typing-indicator">Grok is thinking…</div>}
      </div>

      {permission ? <PermissionCard request={permission} onSelect={onPermissionSelect} /> : null}

      {filteredCommands.length > 0 ? (
        <div className="slash-menu">
          {filteredCommands.map((c) => (
            <button key={c.id} className="slash-item" onClick={() => insertCommand(c.id)}>
              <Command size={14} />
              <div>
                <div className="slash-cmd">{c.id}</div>
                <div className="slash-desc">{c.label}</div>
              </div>
            </button>
          ))}
        </div>
      ) : null}

      <div className="composer">
        <textarea
          ref={textareaRef}
          rows={1}
          value={input}
          onChange={onInputChange}
          onKeyDown={onKeyDown}
          placeholder="Ask Grok or type / for commands…"
        />
        <button className="send-button" disabled={!input.trim() || thinking} onClick={() => submit(input)}>
          <Send size={20} />
        </button>
      </div>

      {showModelPicker ? (
        <ModelPicker
          models={availableModels}
          selected={model}
          onSelect={handleModelSelect}
          onClose={() => setShowModelPicker(false)}
        />
      ) : null}

      {showSettings ? (
        <Settings onClose={() => setShowSettings(false)} onConnect={onConnectSaved} currentUrl={url} />
      ) : null}
    </div>
  );
}
