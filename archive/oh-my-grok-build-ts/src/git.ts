import spawner from "./spawner.js";

const DEFAULT_MAX_BYTES = 100_000;

export function gitOutput(cwd: string, args: string[], maxBytes = DEFAULT_MAX_BYTES): Promise<string> {
  return new Promise((resolve, reject) => {
    let output = "";
    let stderr = "";
    let killed = false;
    const proc = spawner.spawn("git", args, { cwd });
    proc.stdout?.setEncoding("utf8");
    proc.stdout?.on("data", (chunk: string) => {
      if (killed) return;
      output += chunk;
      if (Buffer.byteLength(output, "utf8") > maxBytes) {
        killed = true;
        proc.kill("SIGTERM");
        output += "\n[truncated: output exceeded size limit]";
      }
    });
    proc.stderr?.setEncoding("utf8");
    proc.stderr?.on("data", (chunk: string) => {
      stderr += chunk;
    });
    proc.on("error", reject);
    proc.on("exit", (code) => {
      if (code !== 0 && code !== null) {
        const message = output.trim() || stderr.trim() || "(no output)";
        const err = new Error(`git ${args.join(" ")} exited with code ${code}: ${message}`) as Error & {
          code?: number | null;
        };
        err.code = code;
        reject(err);
        return;
      }
      resolve(output);
    });
  });
}

export function gitStatusShort(cwd: string, maxBytes = DEFAULT_MAX_BYTES): Promise<string> {
  return gitOutput(cwd, ["status", "--short"], maxBytes);
}

export function gitDiff(cwd: string, maxBytes = DEFAULT_MAX_BYTES): Promise<string> {
  return gitOutput(cwd, ["diff"], maxBytes);
}

export function isNotGitRepo(err: unknown): boolean {
  if (err instanceof Error) {
    if ((err as Error & { code?: number | null }).code === 128) return true;
    if (err.message.includes("not a git repository")) return true;
  }
  return false;
}
