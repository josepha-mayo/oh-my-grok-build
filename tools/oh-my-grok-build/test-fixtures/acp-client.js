import { WebSocket } from "ws";

const url = process.argv[2];
const ws = new WebSocket(url);

let id = 0;
function send(method, params) {
  ws.send(JSON.stringify({ jsonrpc: "2.0", id: ++id, method, params }));
}

function sendNotif(method, params) {
  ws.send(JSON.stringify({ jsonrpc: "2.0", method, params }));
}

ws.on("open", () => {
  send("initialize", { protocolVersion: 1, clientCapabilities: { terminal: true, fs: { readTextFile: true, writeTextFile: true } } });
});

ws.on("message", (data) => {
  console.log("<<", data.toString());
  const msg = JSON.parse(data.toString());
  if (msg.method === "authenticate") {
    send("authenticate", { methodId: "xai.api_key", _meta: { headless: true } });
  } else if (msg.id === id && msg.method === undefined) {
    // response
    if (id === 1) {
      send("authenticate", { methodId: "xai.api_key", _meta: { headless: true } });
    } else if (id === 2) {
      send("session/new", { cwd: ".", mcpServers: [], _meta: {} });
    } else if (id === 3) {
      send("session/set_config_option", { sessionId: "sess-1", configId: "mode", value: "ask" });
    } else if (id === 4) {
      send("session/prompt", { sessionId: "sess-1", prompt: [{ type: "text", text: "hello" }] });
      setTimeout(() => ws.close(), 500);
    }
  }
});

ws.on("error", (err) => console.error("ws error", err.message));
ws.on("close", () => console.log("closed"));
