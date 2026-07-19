export interface ConnectorConfig {
  name: string;
  type: "opencode" | "codex" | "claude" | "hermes" | "pi" | "omp";
  url?: string;
  command?: string;
  cwd?: string;
  env?: Record<string, string>;
  secret?: string;
}

export interface ConnectorResult {
  text: string;
  usage?: Record<string, unknown>;
  cost?: number;
  outputFiles?: string[];
}

export interface Connector {
  readonly config: ConnectorConfig;
  run(prompt: string): Promise<ConnectorResult>;
  close?(): Promise<void>;
}

export interface ConnectorRegistry {
  connectors: Record<string, ConnectorConfig>;
}
