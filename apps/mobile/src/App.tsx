import { useState, useRef, useCallback, useEffect } from "react";
import {
  AcpClient,
  type AcpPermissionRequest,
  type AcpUpdate,
  type AcpPermissionResponse,
  type AcpSessionConfigOption,
} from "./acp/client";
import { ConnectionScreen } from "./components/ConnectionScreen";
import { ChatScreen, type Message, SLASH_COMMANDS } from "./components/ChatScreen";
import { SessionList, type SessionListItem } from "./components/SessionList";
import type { ToolOutputData } from "./components/ToolOutput";
import type { ReasoningEffort } from "./components/EffortPicker";
import { notifyCompletion, requestNotificationPermission } from "./notifications";
import { persistGet, persistSet, secureGetJson, secureSetJson } from "./storage";
import { loadProviders, findProviderApiKey } from "./providers";
import "./App.css";

type View = "connect" | "chat";
type ConnectionStatus = "connecting" | "connected" | "disconnected";

const CONNECTIONS_KEY = "connections";
const SERVER_KEYS_KEY = "serverKeys";

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

async function getServerKey(safeUrl: string): Promise<string | undefined> {
  try {
    const map = (await secureGetJson<Record<string, string>>(SERVER_KEYS_KEY)) ?? {};
    return map[safeUrl];
  } catch {
    return undefined;
  }
}

async function setServerKey(safeUrl: string, secret: string | undefined): Promise<void> {
  try {
    const map = (await secureGetJson<Record<string, string>>(SERVER_KEYS_KEY)) ?? {};
    if (secret) map[safeUrl] = secret;
    else delete map[safeUrl];
    await secureSetJson(SERVER_KEYS_KEY, map);
  } catch {
    // ignore
  }
}

async function restoreSecret(url: string): Promise<string> {
  try {
    const u = new URL(url);
    if (u.searchParams.has("server-key")) return url;
    const safeUrl = stripSecret(url);
    const secret = await getServerKey(safeUrl);
    if (secret) u.searchParams.set("server-key", secret);
    return u.toString();
  } catch {
    return url;
  }
}

function loadLastUrl(): string {
  try {
    const parsed = persistGet<{ url?: string }[]>(CONNECTIONS_KEY) ?? [];
    if (Array.isArray(parsed) && parsed.length > 0) {
      return parsed[0].url ?? "";
    }
  } catch {
    // ignore
  }
  return "";
}

async function saveConnection(url: string): Promise<void> {
  const safeUrl = stripSecret(url);
  const secret = extractSecret(url) ?? (await getServerKey(safeUrl));
  await setServerKey(safeUrl, secret);
  try {
    const parsed = persistGet<{ url: string; name: string }[]>(CONNECTIONS_KEY) ?? [];
    const list = Array.isArray(parsed)
      ? parsed.filter((c): c is { url: string; name: string } => typeof c.url === "string")
      : [];
    const without = list.filter((c) => c.url !== safeUrl);
    const next = [{ url: safeUrl, name: safeUrl }, ...without].slice(0, 20);
    persistSet(CONNECTIONS_KEY, next);
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

function normalizeReasoningEffort(raw: string | undefined): ReasoningEffort | undefined {
  const map: Record<string, ReasoningEffort> = {
    low: "low",
    medium: "medium",
    high: "high",
    xhigh: "max",
    max: "max",
    minimal: "low",
  };
  return raw ? map[raw.toLowerCase()] : undefined;
}

function shellQuote(input: string): string {
  if (!input) return "''";
  return `'${input.replace(/'/g, "'\"'\"'")}'`;
}

function isLocalOrPrivateHost(host: string): boolean {
  const h = host.toLowerCase();
  if (h === "localhost" || h === "127.0.0.1" || h === "[::1]" || h === "::1") return true;
  if (h.startsWith("10.") || h.startsWith("192.168.") || /^172\.(1[6-9]|2[0-9]|3[0-1])\./.test(h)) return true;
  if (h.startsWith("fc") || h.startsWith("fd") || h.startsWith("fe80:")) return true;
  return false;
}

function isUrlSafeForSecrets(url: string): boolean {
  try {
    const u = new URL(url);
    if (u.protocol !== "ws:" && u.protocol !== "wss:") return false;
    return isLocalOrPrivateHost(u.hostname);
  } catch {
    return false;
  }
}

function isValidPairingUrl(url: string): boolean {
  try {
    const u = new URL(url);
    if (u.protocol === "wss:") return true;
    if (u.protocol === "ws:") return isLocalOrPrivateHost(u.hostname);
    return false;
  } catch {
    return false;
  }
}

function applyModeFromConfigOption(
  options: AcpSessionConfigOption[] | undefined,
  setYolo: (v: boolean) => void,
  setAuto: (v: boolean) => void
): void {
  const modeOption = options?.find((o) => o.category === "mode");
  if (modeOption && typeof modeOption.currentValue === "string") {
    const current = modeOption.currentValue;
    setYolo(current !== "ask" && current !== "plan");
    setAuto(current === "autonomous");
  }
}

export default function App() {
  const [view, setView] = useState<View>("connect");
  const [url, setUrl] = useState(loadLastUrl);
  const [model, setModel] = useState("grok-build");
  const [effort, setEffort] = useState<ReasoningEffort>("medium");
  const [yolo, setYolo] = useState(false);
  const [auto, setAuto] = useState(false);
  const [messages, setMessages] = useState<Message[]>([]);
  const [thinking, setThinking] = useState(false);
  const [permission, setPermission] = useState<AcpPermissionRequest | null>(null);
  const [askUserQuestion, setAskUserQuestion] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [connectionStatus, setConnectionStatus] = useState<ConnectionStatus>("disconnected");
  const [availableModels, setAvailableModels] = useState<string[]>([]);
  const [showSessionList, setShowSessionList] = useState(false);
  const [sessions, setSessions] = useState<SessionListItem[]>([]);

  const clientRef = useRef<AcpClient | null>(null);
  const permissionResolver = useRef<((value: AcpPermissionResponse) => void) | null>(null);
  const askUserResolver = useRef<((value: string | null) => void) | null>(null);
  const closingRef = useRef(false);
  const loopRef = useRef<LoopState | null>(null);
  const sessionIdRef = useRef<string>("");
  const cwdRef = useRef<string>("");

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
        case "model_changed":
        case "config_option_update": {
          if (typeof update.model_id === "string") {
            setModel(update.model_id);
          }
          const nextEffort = normalizeReasoningEffort(update.reasoning_effort);
          if (nextEffort) {
            setEffort(nextEffort);
          }
          if (update.configOptions) {
            applyModeFromConfigOption(update.configOptions, setYolo, setAuto);
          }
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

  const handlePermissionSelect = useCallback((optionId: string | null) => {
    setPermission(null);
    if (optionId) {
      permissionResolver.current?.({ outcome: { outcome: "selected", optionId } });
    } else {
      permissionResolver.current?.({ outcome: { outcome: "cancelled" } });
    }
    permissionResolver.current = null;
  }, []);

  const handleAskUserSubmit = useCallback((value: string) => {
    askUserResolver.current?.(value);
    askUserResolver.current = null;
    setAskUserQuestion(null);
  }, []);

  const handleAskUserCancel = useCallback(() => {
    askUserResolver.current?.(null);
    askUserResolver.current = null;
    setAskUserQuestion(null);
  }, []);

  const handleOpenSessions = useCallback(async () => {
    const client = clientRef.current;
    if (!client) return;
    try {
      const res = await client.listSessions();
      setSessions(
        (res.sessions ?? []).map((s) => ({
          sessionId: s.sessionId,
          title: s.title,
          cwd: s.cwd,
          updatedAt: s.updatedAt,
        }))
      );
      setShowSessionList(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  const handleSessionSelect = useCallback(async (id: string) => {
    const client = clientRef.current;
    if (!client) return;
    setShowSessionList(false);
    setMessages([]);
    setThinking(true);
    try {
      await client.loadSession(id);
      sessionIdRef.current = id;
      setThinking(false);
    } catch (err) {
      setThinking(false);
      setError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  const handleModelsUpdate = useCallback((models: string[]) => {
    setAvailableModels((prev) => Array.from(new Set([...prev, ...models])));
  }, []);

  const closeCurrentSession = useCallback(async () => {
    const client = clientRef.current;
    const sid = sessionIdRef.current;
    if (client && sid) {
      try {
        await client.closeSession(sid);
      } catch {
        // ignore
      }
    }
    sessionIdRef.current = "";
  }, []);

  const startSessionWithProfile = useCallback(
    async (
      client: AcpClient,
      modelId: string,
      yoloMode: boolean,
      autoMode: boolean,
      reasoningEffort: ReasoningEffort,
      cwd: string
    ) => {
      if (!cwd) {
        throw new Error("Server did not provide an absolute working directory");
      }
      const { sessionId: sid, configOptions } = await client.newSession(
        cwd,
        [],
        { modelId, yoloMode, autoMode, reasoningEffort },
        60_000
      );
      sessionIdRef.current = sid;
      setThinking(false);
      loopRef.current = null;

      applyModeFromConfigOption(configOptions, setYolo, setAuto);

      try {
        await client.setModelWithEffort(sid, modelId, reasoningEffort, 60_000);
      } catch (err) {
        console.warn("Failed to set model/effort:", err);
      }

      const desiredMode = autoMode ? "autonomous" : yoloMode ? "code" : "ask";
      try {
        await client.setMode(sid, desiredMode, 60_000);
      } catch (err) {
        console.warn("Failed to set mode:", err);
      }

      return sid;
    },
    []
  );

  const restartSessionWithProfile = useCallback(
    async (client: AcpClient, nextYolo: boolean, nextAuto: boolean, nextEffort: ReasoningEffort, nextModel = model) => {
      await closeCurrentSession();
      return startSessionWithProfile(client, nextModel, nextYolo, nextAuto, nextEffort, cwdRef.current);
    },
    [closeCurrentSession, model, startSessionWithProfile]
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

  const handleModelChange = useCallback(
    async (modelId: string) => {
      const client = clientRef.current;
      if (client && sessionIdRef.current) {
        try {
          await client.setModelWithEffort(sessionIdRef.current, modelId, effort, 60_000);
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
    [appendMessage, effort]
  );

  const handleEffortChange = useCallback(
    async (e: ReasoningEffort) => {
      const client = clientRef.current;
      setEffort(e);
      if (client && sessionIdRef.current) {
        try {
          const effortSet = await client.setEffort(sessionIdRef.current, e, 60_000);
          if (effortSet) {
            appendMessage("agent", { text: `Reasoning effort set to ${e}.` });
          } else {
            await restartSessionWithProfile(client, yolo, auto, e);
            appendMessage("agent", { text: `Session restarted with reasoning effort ${e}.` });
          }
        } catch (err) {
          appendMessage("agent", {
            text: `Failed to set effort: ${err instanceof Error ? err.message : String(err)}`,
          });
        }
      } else {
        appendMessage("agent", { text: `Effort preference set to ${e}.` });
      }
    },
    [appendMessage, model, yolo, auto, restartSessionWithProfile]
  );

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
          case "/yolo": {
            const next = !yolo;
            setYolo(next);
            const desiredMode = auto ? "autonomous" : next ? "code" : "ask";
            appendMessage("agent", { text: `Always-approve ${next ? "enabled" : "disabled"}.` });
            let modeSet = false;
            try {
              modeSet = await client.setMode(currentSessionId, desiredMode, 60_000);
            } catch (err) {
              appendMessage("agent", {
                text: `Failed to set mode: ${err instanceof Error ? err.message : String(err)}`,
              });
            }
            if (modeSet) {
              appendMessage("agent", { text: `Mode set to ${desiredMode}.` });
            } else {
              try {
                await restartSessionWithProfile(client, next, auto, effort);
                appendMessage("agent", { text: `Session restarted with always-approve ${next ? "on" : "off"}.` });
              } catch (err) {
                appendMessage("agent", {
                  text: `Failed to switch mode: ${err instanceof Error ? err.message : String(err)}`,
                });
              }
            }
            return;
          }
          case "/autonomous":
          case "/devin autonomous": {
            const next = !auto;
            setAuto(next);
            const desiredMode = next ? "autonomous" : yolo ? "code" : "ask";
            appendMessage("agent", { text: `Auto-approve ${next ? "enabled" : "disabled"}.` });
            let modeSet = false;
            try {
              modeSet = await client.setMode(currentSessionId, desiredMode, 60_000);
            } catch (err) {
              appendMessage("agent", {
                text: `Failed to set mode: ${err instanceof Error ? err.message : String(err)}`,
              });
            }
            if (modeSet) {
              appendMessage("agent", { text: `Mode set to ${desiredMode}.` });
            } else {
              try {
                await restartSessionWithProfile(client, yolo, next, effort);
                appendMessage("agent", { text: `Session restarted with auto-approve ${next ? "on" : "off"}.` });
              } catch (err) {
                appendMessage("agent", {
                  text: `Failed to switch mode: ${err instanceof Error ? err.message : String(err)}`,
                });
              }
            }
            return;
          }
          case "/plan": {
            let planSet = false;
            try {
              planSet = await client.setMode(currentSessionId, "plan", 60_000);
            } catch (err) {
              appendMessage("agent", {
                text: `Failed to set plan mode: ${err instanceof Error ? err.message : String(err)}`,
              });
            }
            if (planSet) {
              setYolo(false);
              setAuto(false);
              appendMessage("agent", { text: "Switched to plan mode." });
            } else {
              appendMessage("agent", { text: "Plan mode cannot be toggled from mobile yet." });
            }
            return;
          }
          case "/model":
            if (arg) {
              await handleModelChange(arg);
            } else {
              appendMessage("agent", { text: "Usage: /model <model-id>" });
            }
            return;
          case "/effort":
            if (arg && ["low", "medium", "high", "max"].includes(arg)) {
              const e = arg as ReasoningEffort;
              setEffort(e);
              let effortSet = false;
              try {
                effortSet = await client.setEffort(currentSessionId, e, 60_000);
              } catch (err) {
                appendMessage("agent", {
                  text: `Failed to set effort: ${err instanceof Error ? err.message : String(err)}`,
                });
              }
              if (effortSet) {
                appendMessage("agent", { text: `Reasoning effort set to ${e}.` });
              } else {
                try {
                  await restartSessionWithProfile(client, yolo, auto, e);
                  appendMessage("agent", { text: `Session restarted with effort ${e}.` });
                } catch (err) {
                  appendMessage("agent", {
                    text: `Failed to set effort: ${err instanceof Error ? err.message : String(err)}`,
                  });
                }
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
            const swarmCmd = `omgb swarm ${shellQuote(arg)}`;
            appendMessage("agent", {
              text: `Swarm mode runs on the desktop CLI. Copy and run:\n\`\`\`\n${swarmCmd}\n\`\`\``,
            });
            return;
          }
          case "/sessions": {
            void handleOpenSessions();
            return;
          }
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
    [
      appendMessage,
      yolo,
      auto,
      model,
      effort,
      handleModelChange,
      restartSessionWithProfile,
      matchSlashCommand,
      handleOpenSessions,
    ]
  );

  const connect = useCallback(
    async (connectUrl: string, switchView = true, clearHistory = true) => {
      setError(null);
      if (!isValidPairingUrl(connectUrl)) {
        setError("Only ws://localhost/private or wss:// URLs are allowed for pairing.");
        setConnectionStatus("disconnected");
        return;
      }
      closingRef.current = false;
      clientRef.current?.close();
      clientRef.current = null;
      const safeUrl = stripSecret(connectUrl);
      const fullUrl = await restoreSecret(connectUrl);
      setUrl(safeUrl);
      if (clearHistory) setMessages([]);
      setPermission(null);
      setAskUserQuestion(null);
      setConnectionStatus("connecting");

      let client: AcpClient;
      const isCurrentClient = () => clientRef.current === client;
      const onOpen = () => {
        if (!isCurrentClient()) return;
        closingRef.current = false;
        setError(null);
        setConnectionStatus("connected");
      };
      const onClose = () => {
        if (!isCurrentClient()) return;
        setConnectionStatus("disconnected");
        if (!closingRef.current) setError("Connection closed");
      };
      const onError = (err: Error) => {
        if (!isCurrentClient()) return;
        setConnectionStatus("disconnected");
        setError(err.message);
      };
      client = new AcpClient(fullUrl, {
        onOpen,
        onClose,
        onError,
        onUpdate: handleUpdate,
        onPermission: handlePermission,
        onModelsUpdate: handleModelsUpdate,
        onAskUser: async (question) => {
          return new Promise((resolve) => {
            askUserResolver.current = resolve;
            setAskUserQuestion(question);
          });
        },
      });
      clientRef.current = client;

      try {
        const init = await client.initialize(1, {}, 30_000);

        const meta = init.meta;
        const serverCwd = meta?.currentWorkingDirectory as string | undefined;
        const modelState = meta?.modelState as
          { currentModelId?: string; availableModels?: { modelId?: string; name?: string }[] } | undefined;

        const initialModelId = modelState?.currentModelId ?? model;
        const initialModels =
          modelState?.availableModels?.map((m) => m.modelId ?? m.name ?? "").filter((m): m is string => !!m) ?? [];
        const available = Array.from(new Set([initialModelId, ...initialModels])).filter((m): m is string => !!m);

        if (initialModelId) setModel(initialModelId);
        if (available.length) setAvailableModels(available);
        if (serverCwd) cwdRef.current = serverCwd;

        const interactiveIds = new Set(["grok.com", "oauth", "browser"]);
        const authMethod =
          init.authMethods?.find((m) => m.id === "xai.api_key") ??
          init.authMethods?.find((m) => !interactiveIds.has(m.id.toLowerCase())) ??
          init.authMethods?.[0];
        if (authMethod) {
          const authMeta: Record<string, unknown> = {};
          if (isUrlSafeForSecrets(fullUrl)) {
            const providers = await loadProviders();
            const providerKey = findProviderApiKey(providers, initialModelId, authMethod.id);
            if (providerKey) {
              authMeta.apiKey = providerKey.value;
              authMeta[providerKey.key] = providerKey.value;
            }
          } else {
            console.warn("Refusing to send provider API key over a non-local WebSocket URL:", stripSecret(fullUrl));
          }
          await client.authenticate(authMethod, authMeta, 60_000);
        }
        await startSessionWithProfile(client, initialModelId, yolo, auto, effort, cwdRef.current);
        setConnectionStatus("connected");
        if (switchView) setView("chat");
        await saveConnection(connectUrl);
        void requestNotificationPermission();
      } catch (err) {
        closingRef.current = true;
        client.close();
        clientRef.current = null;
        setConnectionStatus("disconnected");
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [handleUpdate, handlePermission, handleModelsUpdate, model, yolo, auto, effort, startSessionWithProfile]
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
    cwdRef.current = "";
    setView("connect");
    setMessages([]);
    setPermission(null);
    setAskUserQuestion(null);
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
        yolo={yolo || auto}
        messages={messages}
        thinking={thinking}
        permission={permission}
        askUser={askUserQuestion}
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
        onAskUserSubmit={handleAskUserSubmit}
        onAskUserCancel={handleAskUserCancel}
        onOpenSessions={handleOpenSessions}
      />
      {showSessionList ? (
        <SessionList sessions={sessions} onSelect={handleSessionSelect} onClose={() => setShowSessionList(false)} />
      ) : null}
      {error ? <div className="toast error">{error}</div> : null}
    </div>
  );
}
