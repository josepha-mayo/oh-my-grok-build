import { spawn as cpSpawn, spawnSync as cpSpawnSync } from "node:child_process";

export function spawn(...args: Parameters<typeof cpSpawn>): ReturnType<typeof cpSpawn> {
  return cpSpawn(...args);
}

export function spawnSync(...args: Parameters<typeof cpSpawnSync>): ReturnType<typeof cpSpawnSync> {
  return cpSpawnSync(...args);
}

export default { spawn, spawnSync };
