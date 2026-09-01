export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * Whether `value` carries no key outside `allowed`.
 *
 * Generic over the wire type so the allowlist has to be spelled with that type's
 * own keys: a field renamed in Rust drops out of `keyof` and the call below
 * fails to compile. Without that, a rename left the allowlist naming the old key
 * and rejecting the new one, so the validator would reject every payload and the
 * surface would simply stop appearing — with nothing failing.
 */
export function onlyKeys<Wire>(
  value: Record<string, unknown>,
  allowed: readonly (keyof Wire & string)[],
): boolean {
  const set = new Set<string>(allowed);
  return Object.keys(value).every((key) => set.has(key));
}
