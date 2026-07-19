export function isRateLimited(text: string): boolean {
  return /rate.?limit|429|too many requests|rate_limit|ratelimit/i.test(text);
}

export function formatRateLimitMessage(): string {
  return "Looks like you hit a rate limit. Please wait a moment and try again.";
}
