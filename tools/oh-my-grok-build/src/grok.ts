import { spawn } from "child_process";
import { EventEmitter } from "events";
import type { GrokEvent } from "./types.js";

function findGrok(): string {
  return process.env.GROK_PATH ?? "grok";
}

export function runGrok(
  prompt: string,
  options: {
    cwd?: string;
    model?: string;
    yolo?: boolean;
    sessionId?: string;
    resume?: string;
    outputFormat?: "plain" | "json" | "streaming-json";
    extraArgs?: string[];
  } = {}
): EventEmitter {
  const emitter = new EventEmitter();
  const args = ["-p", prompt];
  if (options.cwd) args.push("--cwd", options.cwd);
  if (options.model) args.push("-m", options.model);
  if (options.yolo) args.push("--yolo");
  if (options.sessionId) args.push("-s", options.sessionId);
  if (options.resume) args.push("-r", options.resume);
  if (options.outputFormat) args.push("--output-format", options.outputFormat);
  if (options.extraArgs) args.push(...options.extraArgs);

  const proc = spawn(findGrok(), args, { env: process.env, cwd: options.cwd });
  let buffer = "";

  proc.stdout?.on("data", (chunk: Buffer) => {
    buffer += chunk.toString("utf8");
    if (options.outputFormat === "streaming-json") {
      const lines = buffer.split("\n");
      buffer = lines.pop() ?? "";
      for (const line of lines) {
        if (!line.trim()) continue;
        try {
          const ev = JSON.parse(line) as GrokEvent;
          emitter.emit("event", ev);
        } catch {
          emitter.emit("event", { type: "text", data: line } as GrokEvent);
        }
      }
    }
  });

  proc.stdout?.on("end", () => {
    if (options.outputFormat === "streaming-json" && buffer.trim()) {
      try {
        const ev = JSON.parse(buffer) as GrokEvent;
        emitter.emit("event", ev);
      } catch { /* ignore trailing noise */ }
    }
    if (options.outputFormat !== "streaming-json" && buffer) {
      emitter.emit("event", { type: "text", data: buffer } as GrokEvent);
    }
    emitter.emit("end");
  });

  proc.stderr?.on("data", (chunk: Buffer) => {
    emitter.emit("event", { type: "error", data: chunk.toString("utf8") } as GrokEvent);
  });

  proc.on("error", (err) => {
    emitter.emit("event", { type: "error", message: err.message } as GrokEvent);
    emitter.emit("end");
  });

  return emitter;
}

export async function runGrokOnce(prompt: string, opts: Parameters<typeof runGrok>[1] = {}): Promise<string> {
  return new Promise((resolve, reject) => {
    const out: string[] = [];
    const child = runGrok(prompt, { ...opts, outputFormat: "plain" });
    child.on("event", (ev: GrokEvent) => {
      if (ev.type === "text" && ev.data) out.push(ev.data);
      if (ev.type === "error") reject(new Error(ev.data ?? ev.message ?? "grok failed"));
    });
    child.on("end", () => resolve(out.join("")));
  });
}
