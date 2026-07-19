/**
 * Only allow user-supplied environment variables that look like API keys.
 * Never let connector/MCP configs override PATH, LD_PRELOAD, SHELL, HOME, etc.
 */
export function sanitizeUserEnv(env: Record<string, string> | undefined): Record<string, string> {
  const safe: Record<string, string> = {};
  if (!env) return safe;
  for (const [key, value] of Object.entries(env)) {
    if (/^[_A-Z0-9]+_API_KEY$/i.test(key)) {
      safe[key] = value;
    }
  }
  return safe;
}
