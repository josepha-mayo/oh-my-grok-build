#!/usr/bin/env node
import { Command } from "commander";
import { runGrok, runGrokOnce } from "./grok.js";
import { startRelay } from "./server.js";
import { getTaste, listTaste, removeTaste, setTaste, learnFromSession } from "./taste.js";
import { listSessions } from "./session.js";
import { runTeam, streamTeam } from "./team.js";
import type { GrokEvent } from "./types.js";

const program = new Command("omgb").description("Oh My Grok Build — productivity layer for grok").version("0.1.0");

program
  .command("serve")
  .description("Start the mobile/remote relay server")
  .option("-p, --port <port>", "relay port", "3001")
  .action((opts) => startRelay(Number(opts.port)));

program
  .command("exec <prompt>")
  .description("Run a single headless grok prompt")
  .option("-m, --model <model>", "model override")
  .option("--yolo", "auto-approve tools", false)
  .option("-c, --cwd <cwd>", "working directory")
  .option("-s, --stream", "stream output", false)
  .option("-r, --resume <id>", "resume session id")
  .action(async (prompt, opts) => {
    if (opts.stream) {
      const child = runGrok(prompt, {
        cwd: opts.cwd,
        model: opts.model,
        yolo: opts.yolo,
        resume: opts.resume,
        outputFormat: "streaming-json",
      });
      child.on("event", (ev: GrokEvent) => console.log(JSON.stringify(ev)));
      child.on("end", () => process.exit(0));
    } else {
      const out = await runGrokOnce(prompt, { cwd: opts.cwd, model: opts.model, yolo: opts.yolo, resume: opts.resume });
      console.log(out);
    }
  });

const taste = program.command("taste").description("Manage taste packages");

taste
  .command("list")
  .description("List taste packages")
  .action(() => console.log(JSON.stringify(listTaste(), null, 2)));

taste
  .command("show <name>")
  .description("Show a taste package")
  .action((name) => {
    const pkg = getTaste(name);
    if (!pkg) { console.error(`Taste package ${name} not found`); process.exit(1); }
    console.log(JSON.stringify(pkg, null, 2));
  });

taste
  .command("set")
  .description("Set a taste package from stdin")
  .action(async () => {
    const stdin = await readStdin();
    setTaste(JSON.parse(stdin));
  });

taste
  .command("rm <name>")
  .description("Remove a taste package")
  .action((name) => removeTaste(name));

taste
  .command("learn <sessionDir>")
  .description("Derive taste from a Grok session directory")
  .action((sessionDir) => console.log(JSON.stringify(learnFromSession(sessionDir), null, 2)));

program
  .command("team <prompt>")
  .description("Run the same prompt through N grok agents in parallel")
  .option("-n, --agents <n>", "number of agents", "3")
  .option("-m, --model <model>", "model override")
  .option("--yolo", "auto-approve tools", false)
  .option("-c, --cwd <cwd>")
  .action(async (prompt, opts) => {
    const n = Number(opts.agents);
    const gen = streamTeam(prompt, { cwd: opts.cwd, model: opts.model, yolo: opts.yolo, agents: n });
    for await (const ev of gen) {
      console.log(JSON.stringify(ev));
    }
  });

program
  .command("sessions")
  .description("List recent Grok sessions")
  .option("-l, --limit <limit>", "limit", "20")
  .action((opts) => console.log(JSON.stringify(listSessions(Number(opts.limit)), null, 2)));

program.parse();

async function readStdin(): Promise<string> {
  let data = "";
  process.stdin.setEncoding("utf8");
  for await (const chunk of process.stdin) data += chunk;
  return data;
}
