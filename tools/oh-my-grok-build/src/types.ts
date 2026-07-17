export interface GrokEvent {
  type: "text" | "thought" | "end" | "error";
  data?: string;
  sessionId?: string;
  requestId?: string;
  usage?: object;
  stopReason?: string;
  message?: string;
}

export interface TastePackage {
  name: string;
  category: string;
  confidence: number;
  learned: string[];
}

export interface RelayClient {
  id: string;
  code: string;
  ws: import("ws").WebSocket;
  cwd: string;
}
