export function formatProviderError(status: number, body: string): string {
  if (status === 429) {
    return "Rate limit reached. Please wait a moment and try again.";
  }
  return `${status}: ${body.slice(0, 200)}`;
}
