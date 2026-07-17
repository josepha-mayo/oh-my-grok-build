import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { PassThrough } from "node:stream";

export function makeTempDir(): string {
  return mkdtempSync(join(tmpdir(), "omgb-test-"));
}

export function setupOmgHome(): string {
  const dir = makeTempDir();
  process.env.OMGB_HOME = dir;
  process.env.GROK_HOME = join(dir, ".grok");
  return dir;
}

export function cleanupOmgHome(dir: string): void {
  delete process.env.OMGB_HOME;
  delete process.env.GROK_HOME;
  rmSync(dir, { recursive: true, force: true });
}

interface StdioLike extends PassThrough {
  on: (event: string, cb: (...args: any[]) => void) => this;
}

export interface FakeProcess {
  pid: number;
  stdin: StdioLike;
  stdout: StdioLike;
  stderr: StdioLike;
  killed: boolean;
  exitCode: number | null;
  on: (event: string, cb: (...args: any[]) => void) => this;
  unref: () => void;
  kill: (signal?: string | number) => boolean;
  emit: (event: string, ...args: any[]) => void;
  finish: (code?: number | null) => void;
}

export function fakeProcess(pid = 1): FakeProcess {
  const listeners: Record<string, ((...args: any[]) => void)[]> = {};
  const stdin = new PassThrough() as StdioLike;
  const stdout = new PassThrough() as StdioLike;
  const stderr = new PassThrough() as StdioLike;

  const proc: FakeProcess = {
    pid,
    stdin,
    stdout,
    stderr,
    killed: false,
    exitCode: null,
    on: (event, cb) => {
      listeners[event] = listeners[event] ?? [];
      listeners[event].push(cb);
      return proc;
    },
    unref: () => {},
    kill: (signal?) => {
      proc.killed = true;
      setImmediate(() => proc.emit("exit", 1));
      return true;
    },
    emit: (event, ...args) => {
      for (const cb of listeners[event] ?? []) cb(...args);
    },
    finish: (code = 0) => {
      proc.exitCode = code;
      setImmediate(() => proc.emit("exit", code));
    },
  };
  return proc;
}
