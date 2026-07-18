export interface AcpMessage {
  jsonrpc: "2.0";
  id?: string | number | null;
  method?: string;
  params?: unknown;
  result?: unknown;
  error?: { code: number; message: string };
}

export interface AcpUpdate {
  sessionUpdate?: string;
  content?: { text?: string };
  title?: string;
  status?: string;
  configOptions?: unknown;
}

export interface AcpPermissionOption {
  optionId: string;
  kind?: string;
  title?: string;
  description?: string;
}

export interface AcpPermissionRequest {
  sessionId: string;
  toolCall: {
    toolCallId: string;
    title?: string;
    command?: string;
    [key: string]: unknown;
  };
  prompt: string;
  options: AcpPermissionOption[];
}

export interface AcpPermissionResponse {
  outcome:
    | { outcome: "selected"; optionId: string }
    | { outcome: "cancelled" }
    | { outcome: string; optionId?: string };
}

export interface AcpSessionConfigOptionValue {
  value: string;
  name?: string;
}

export interface AcpSessionConfigOption {
  id: string;
  category: string;
  name?: string;
  options?: AcpSessionConfigOptionValue[];
  currentValue?: string | boolean | unknown;
}
