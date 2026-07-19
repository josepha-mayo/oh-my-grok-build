import { spawn as cpSpawn, spawnSync as cpSpawnSync } from "node:child_process";
import type { ChildProcess, SpawnOptions, SpawnSyncOptions } from "node:child_process";
import { parseArgsStringToArgv } from "string-argv";

function withTelemetryDefaults(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  return {
    ...env,
    GROK_TELEMETRY_ENABLED: env.GROK_TELEMETRY_ENABLED ?? "false",
    GROK_TELEMETRY_TRACE_UPLOAD: env.GROK_TELEMETRY_TRACE_UPLOAD ?? "false",
    GROK_TELEMETRY_MIXPANEL_ENABLED: env.GROK_TELEMETRY_MIXPANEL_ENABLED ?? "false",
    GROK_EXTERNAL_OTEL: env.GROK_EXTERNAL_OTEL ?? "false",
  };
}

function resolveGrokCommand<TOptions extends SpawnOptions | SpawnSyncOptions | undefined>(
  command: string,
  args: string[] = [],
  options?: TOptions
): { command: string; args: string[]; options?: TOptions } {
  if (command !== "grok") return { command, args, options };
  const override = process.env.OMGB_GROK_COMMAND?.trim();
  if (!override) return { command, args, options };
  const parts = parseArgsStringToArgv(override);
  if (!parts[0]) return { command, args, options };
  return { command: parts[0], args: [...parts.slice(1), ...args], options };
}

export function spawn(command: string, args: string[] = [], options?: SpawnOptions): ChildProcess {
  const resolved = resolveGrokCommand(command, args, options);
  const baseEnv = resolved.options?.env ?? process.env;
  let env = withTelemetryDefaults(baseEnv);
  if (command === "grok") {
    env = { ...env, GROK_DISABLE_AUTOUPDATER: env.GROK_DISABLE_AUTOUPDATER ?? "1" };
  }
  return cpSpawn(resolved.command, resolved.args, { ...resolved.options, env });
}

export function spawnSync(
  command: string,
  args: string[] = [],
  options?: SpawnSyncOptions
): ReturnType<typeof cpSpawnSync> {
  const resolved = resolveGrokCommand(command, args, options);
  const baseEnv = resolved.options?.env ?? process.env;
  let env = withTelemetryDefaults(baseEnv);
  if (command === "grok") {
    env = { ...env, GROK_DISABLE_AUTOUPDATER: env.GROK_DISABLE_AUTOUPDATER ?? "1" };
  }
  return cpSpawnSync(resolved.command, resolved.args, { ...resolved.options, env });
}

export default { spawn, spawnSync };
