/** Automatic refreshes are intentionally silent in the global fetch wrapper:
 * their owner retains the last good value and retries with backoff. */
export const BACKGROUND_REQUEST_HEADER = "X-Ccteam-Background";

export function backgroundHeaders(headers?: HeadersInit): Headers {
  const next = new Headers(headers);
  next.set(BACKGROUND_REQUEST_HEADER, "1");
  return next;
}
