import { spawn as cpSpawn, spawnSync as cpSpawnSync } from "node:child_process";
import { parseArgsStringToArgv } from "string-argv";

type SpawnArgs = Parameters<typeof cpSpawn>;
type SpawnSyncArgs = Parameters<typeof cpSpawnSync>;

// Default to disabled telemetry unless the user explicitly opted in.
function withTelemetryDefaults(env: NodeJS.ProcessEnv = process.env): NodeJS.ProcessEnv {
  return {
    ...env,
    GROK_TELEMETRY_ENABLED: env.GROK_TELEMETRY_ENABLED ?? "false",
    GROK_TELEMETRY_TRACE_UPLOAD: env.GROK_TELEMETRY_TRACE_UPLOAD ?? "false",
    GROK_TELEMETRY_MIXPANEL_ENABLED: env.GROK_TELEMETRY_MIXPANEL_ENABLED ?? "false",
    GROK_EXTERNAL_OTEL: env.GROK_EXTERNAL_OTEL ?? "false",
  };
}

function resolveGrokCommand(
  command: string,
  args: string[] = [],
  options?: object
): { command: string; args: string[]; options?: object } {
  if (command !== "grok") return { command, args, options };
  const override = process.env.OMGB_GROK_COMMAND?.trim();
  if (!override) return { command, args, options };
  const parts = parseArgsStringToArgv(override);
  if (!parts[0]) return { command, args, options };
  return { command: parts[0], args: [...parts.slice(1), ...args], options };
}

export function spawn(...args: SpawnArgs): ReturnType<typeof cpSpawn> {
  const command = args[0] as string;
  const procArgs = (args[1] as string[] | undefined) ?? [];
  const options = args[2] as object | undefined;
  const resolved = resolveGrokCommand(command, procArgs, options);
  const baseEnv = (resolved.options as { env?: NodeJS.ProcessEnv } | undefined)?.env ?? process.env;
  let env = withTelemetryDefaults(baseEnv);
  if (command === "grok") {
    env = { ...env, GROK_DISABLE_AUTOUPDATER: env.GROK_DISABLE_AUTOUPDATER ?? "1" };
  }
  return cpSpawn(resolved.command, resolved.args, {
    ...resolved.options,
    env,
  } as any);
}

export function spawnSync(...args: SpawnSyncArgs): ReturnType<typeof cpSpawnSync> {
  const command = args[0] as string;
  const procArgs = (args[1] as string[] | undefined) ?? [];
  const options = args[2] as object | undefined;
  const resolved = resolveGrokCommand(command, procArgs, options);
  const baseEnv = (resolved.options as { env?: NodeJS.ProcessEnv } | undefined)?.env ?? process.env;
  let env = withTelemetryDefaults(baseEnv);
  if (command === "grok") {
    env = { ...env, GROK_DISABLE_AUTOUPDATER: env.GROK_DISABLE_AUTOUPDATER ?? "1" };
  }
  return cpSpawnSync(resolved.command, resolved.args, {
    ...resolved.options,
    env,
  } as any);
}

export default { spawn, spawnSync };
