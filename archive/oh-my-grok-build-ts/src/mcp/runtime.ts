import { createInterface } from "node:readline";

export interface McpTool {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
  handler(args: Record<string, unknown>): Promise<unknown>;
}

export interface McpTextContent {
  type: "text";
  text: string;
}

export interface McpImageContent {
  type: "image";
  data: string;
  mimeType: string;
}

export type McpContent = McpTextContent | McpImageContent;

export interface McpServerOptions {
  name: string;
  version?: string;
  tools: McpTool[];
}

export function startMcpServer(options: McpServerOptions): void {
  const rl = createInterface({ input: process.stdin, output: process.stdout, terminal: false });
  let initialized = false;

  const send = (msg: Record<string, unknown>): void => {
    process.stdout.write(JSON.stringify(msg) + "\n");
  };

  const makeResult = (id: number | string, content: McpContent[], isError = false): void => {
    send({ jsonrpc: "2.0", id, result: { content, isError } });
  };

  const handleCall = async (id: number | string, name: string, args: Record<string, unknown>): Promise<void> => {
    const tool = options.tools.find((t) => t.name === name);
    if (!tool) {
      send({ jsonrpc: "2.0", id, error: { code: -32601, message: `Unknown tool: ${name}` } });
      return;
    }
    try {
      const result = await tool.handler(args ?? {});
      if (result && typeof result === "object" && "type" in result) {
        makeResult(id, [result as McpContent]);
      } else if (Array.isArray(result)) {
        makeResult(id, result as McpContent[]);
      } else {
        makeResult(id, [{ type: "text", text: typeof result === "string" ? result : JSON.stringify(result, null, 2) }]);
      }
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      makeResult(id, [{ type: "text", text: message }], true);
    }
  };

  rl.on("line", (line) => {
    let msg: Record<string, unknown>;
    try {
      msg = JSON.parse(line) as Record<string, unknown>;
    } catch {
      return;
    }
    const method = msg.method as string | undefined;
    const id = msg.id as number | string | undefined;

    if (method === "initialize") {
      initialized = true;
      send({
        jsonrpc: "2.0",
        id,
        result: {
          protocolVersion: "2024-11-05",
          capabilities: {},
          serverInfo: { name: options.name, version: options.version ?? "0.1.0" },
        },
      });
      return;
    }

    if (method === "notifications/initialized") {
      return;
    }

    if (!initialized) {
      send({ jsonrpc: "2.0", id, error: { code: -32002, message: "Server not initialized" } });
      return;
    }

    if (method === "tools/list") {
      send({
        jsonrpc: "2.0",
        id,
        result: {
          tools: options.tools.map((t) => ({ name: t.name, description: t.description, inputSchema: t.inputSchema })),
        },
      });
      return;
    }

    if (method === "tools/call") {
      const params = (msg.params ?? {}) as Record<string, unknown>;
      void handleCall(id ?? 0, params.name as string, (params.arguments ?? {}) as Record<string, unknown>);
      return;
    }

    send({ jsonrpc: "2.0", id, error: { code: -32601, message: `Method not found: ${method ?? ""}` } });
  });
}
