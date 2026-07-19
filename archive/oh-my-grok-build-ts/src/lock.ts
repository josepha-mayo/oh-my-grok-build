import { lock } from "proper-lockfile";

const LOCK_OPTIONS = {
  realpath: false,
  retries: {
    retries: 20,
    factor: 1,
    minTimeout: 20,
    maxTimeout: 200,
  },
} as const;

export async function withFileLock<T>(file: string, fn: () => Promise<T>): Promise<T> {
  const release = await lock(file, LOCK_OPTIONS);
  try {
    return await fn();
  } finally {
    await release();
  }
}
