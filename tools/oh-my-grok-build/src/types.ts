/**
 * Core types for the oh-my-grok-build harness.
 */

export interface AcpTextContent {
  type: "text";
  text: string;
}

export interface AcpImageContent {
  type: "image";
  source?: { type: "base64"; media_type: string; data: string };
}

export type AcpPromptPart = AcpTextContent | AcpImageContent;

export interface AcpMessage {
  jsonrpc: "2.0";
  id?: string | number | null;
  method?: string;
  params?: unknown;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
}

export interface AcpAuthMethod {
  id: string;
  [key: string]: unknown;
}

export interface AcpInitializeResponse {
  protocolVersion?: number;
  authMethods?: AcpAuthMethod[];
  meta?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface AcpSessionConfigOptionValue {
  value: string;
  name?: string;
  description?: string;
}

export interface AcpSessionConfigOption {
  id: string;
  name?: string;
  description?: string;
  category?: "model" | "mode" | "thought_level" | "model_config" | string;
  type?: "select" | "boolean" | string;
  currentValue?: string | boolean | unknown;
  options?: AcpSessionConfigOptionValue[];
  [key: string]: unknown;
}

export interface AcpNewSessionResponse {
  sessionId: string;
  configOptions?: AcpSessionConfigOption[];
}

export interface AcpSetConfigOptionResponse {
  configOptions?: AcpSessionConfigOption[];
}

export interface AcpUpdate {
  sessionUpdate:
    | "agent_message_chunk"
    | "agent_thought_chunk"
    | "tool_call"
    | "tool_call_update"
    | "turn_completed"
    | "plan"
    | "stop"
    | "config_option_update"
    | "model_changed"
    | string;
  content?: AcpTextContent | unknown;
  title?: string;
  status?: string;
  stopReason?: string;
  configOptions?: AcpSessionConfigOption[];
  model_id?: string;
  reasoning_effort?: string;
  [key: string]: unknown;
}

export interface AcpPermissionOption {
  optionId: string;
  name: string;
  kind?: string;
}

export interface AcpPermissionRequest {
  sessionId: string;
  toolCall: {
    toolCallId: string;
    title?: string;
    command?: string;
    [key: string]: unknown;
  };
  options: AcpPermissionOption[];
}

export interface AcpPermissionResponse {
  outcome:
    { outcome: "selected"; optionId: string } | { outcome: "cancelled" } | { outcome: string; optionId?: string };
}

export interface ProviderConfig {
  id: string;
  name: string;
  model: string;
  baseUrl: string;
  apiBackend?: "chat_completions" | "responses" | "messages";
  envKey?: string | string[];
  extraHeaders?: Record<string, string>;
  contextWindow?: number;
  temperature?: number;
  topP?: number;
  maxCompletionTokens?: number;
}

export interface OmgConfig {
  defaultModel?: string;
  providers: Record<string, ProviderConfig>;
  relay?: {
    bind?: string;
    port?: number;
    secretEnv?: string;
  };
}

export interface ServerInfo {
  url: string;
  secret: string;
  pid?: number;
  cwd: string;
  close?: () => Promise<void>;
}
