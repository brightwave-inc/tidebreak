export const INITIAL_RECONNECT_DELAY_MS = 250;
export const MAX_RECONNECT_DELAY_MS = 5_000;

/** Next capped exponential backoff, before jitter. */
export function nextReconnectDelay(delayMs: number): number {
  return Math.min(delayMs * 2, MAX_RECONNECT_DELAY_MS);
}

/**
 * Spread reconnects so many phones do not hammer the machine together.
 * Range is 50–150% of the backoff step.
 */
export function jitteredDelay(
  delayMs: number,
  random: () => number = Math.random,
): number {
  const factor = 0.5 + random();
  return Math.max(0, Math.round(delayMs * factor));
}
