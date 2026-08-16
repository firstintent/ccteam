const DEFAULT_BASE_MS = 2_000;
const DEFAULT_CAP_MS = 300_000;
const DEFAULT_JITTER_RATIO = 0.25;

/** Exponential retry delay with bounded symmetric jitter. Failure 1 uses the
 * base delay; the cap remains a true upper bound even after jitter. */
export function retryDelayMs(
  consecutiveFailures: number,
  random: number,
  baseMs: number = DEFAULT_BASE_MS,
  capMs: number = DEFAULT_CAP_MS,
  jitterRatio: number = DEFAULT_JITTER_RATIO,
): number {
  if (consecutiveFailures <= 0) return 0;
  const exponent = Math.min(52, consecutiveFailures - 1);
  const raw = Math.min(capMs, baseMs * 2 ** exponent);
  const boundedRandom = Math.max(0, Math.min(1, random));
  const factor = 1 - jitterRatio + boundedRandom * jitterRatio * 2;
  return Math.min(capMs, Math.max(0, Math.round(raw * factor)));
}
