import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dirname, join } from "node:path";
import { existsSync } from "node:fs";

const __dirname = dirname(fileURLToPath(import.meta.url));

function findScript(): string | undefined {
  const candidates = [join(__dirname, "computer_server.py"), join(__dirname, "..", "src", "mcp", "computer_server.py")];
  for (const c of candidates) {
    if (existsSync(c)) return c;
  }
  return undefined;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const script = findScript();
  if (!script) {
    process.stderr.write("computer_server.py not found\n");
    process.exit(1);
  }

  const python = process.platform === "win32" ? "python" : "python3";
  const proc = spawn(python, [script], { stdio: ["pipe", "pipe", "pipe"] });

  proc.on("error", (err) => {
    process.stderr.write(`Failed to start computer server: ${err.message}\n`);
    process.exit(1);
  });

  proc.stderr?.on("data", (d) => process.stderr.write(d));

  const rl = createInterface({ input: proc.stdout!, output: process.stdout, terminal: false });
  rl.on("line", (line) => process.stdout.write(line + "\n"));

  const stdinRl = createInterface({ input: process.stdin, terminal: false });
  stdinRl.on("line", (line) => {
    if (proc.stdin?.writable) {
      proc.stdin.write(line + "\n");
    }
  });
  stdinRl.on("close", () => {
    proc.stdin?.end();
  });

  proc.on("exit", (code) => process.exit(code ?? 0));
}
