#!/usr/bin/env node
import { createInterface } from "node:readline";

const args = process.argv.slice(2);
const isStdio = args.includes("stdio");
const isAgent = args.includes("agent");

function send(obj) {
  console.log(JSON.stringify(obj));
}

function respond(id, result) {
  send({ jsonrpc: "2.0", id, result });
}

const configOptions = [
  { id: "mode", category: "mode", name: "Mode", options: [{ value: "ask" }, { value: "code" }, { value: "autonomous" }, { value: "plan" }] },
  { id: "model", category: "model", name: "Model", options: [] },
  { id: "thought_level", category: "thought_level", name: "Reasoning effort", options: [{ value: "low" }, { value: "medium" }, { value: "high" }, { value: "max" }] },
];

function onRequest(msg) {
  switch (msg.method) {
    case "initialize":
      respond(msg.id, {
        protocolVersion: 1,
        authMethods: [{ id: "xai.api_key" }],
        sessionConfigOptions: configOptions,
      });
      break;
    case "authenticate":
      respond(msg.id, {});
      break;
    case "session/new":
      respond(msg.id, { sessionId: "sess-1", configOptions });
      break;
    case "session/set_config_option":
      respond(msg.id, { ok: true, configOptions });
      break;
    case "session/set_model":
      respond(msg.id, { ok: true, configOptions });
      break;
    case "session/prompt": {
      const promptText = msg.params?.prompt?.[0]?.text ?? "";
      respond(msg.id, {});
      send({
        jsonrpc: "2.0",
        method: "session/update",
        params: {
          sessionId: "sess-1",
          update: {
            sessionUpdate: "agent_message_chunk",
            content: { type: "text", text: "Fake grok response." },
          },
        },
      });
      if (promptText.includes("side note") || promptText.includes("aside")) {
        send({
          jsonrpc: "2.0",
          method: "session/update",
          params: { sessionId: "sess-1", update: { sessionUpdate: "turn_completed" } },
        });
      } else {
        send({
          jsonrpc: "2.0",
          method: "session/update",
          params: { sessionId: "sess-1", update: { sessionUpdate: "turn_completed" } },
        });
      }
      break;
    }
    case "x.ai/ask_user_question":
      respond(msg.id, { answer: "" });
      break;
    case "session/request_permission":
      respond(msg.id, { outcome: { outcome: "cancelled" } });
      break;
    default:
      respond(msg.id, {});
  }
}

if (isAgent && isStdio) {
  const rl = createInterface({ input: process.stdin, terminal: false });
  rl.on("line", (line) => {
    try {
      const msg = JSON.parse(line);
      if (msg.id !== undefined && msg.method) {
        onRequest(msg);
      }
    } catch {
      // ignore
    }
  });
  rl.on("close", () => process.exit(0));
} else {
  const promptIdx = args.indexOf("-p");
  const prompt = promptIdx !== -1 ? args[promptIdx + 1] : "";
  if (prompt.toLowerCase().includes("decompose")) {
    console.log(JSON.stringify(["subtask one", "subtask two"]));
  } else if (prompt.toLowerCase().includes("research")) {
    console.log("## Proposed patch / implementation\n```python\nprint('hello')\n```");
  } else {
    // Headless grok: print nothing and succeed.
  }
  process.exit(0);
}
