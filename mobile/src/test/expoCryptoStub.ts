// Vitest stand-in for expo-crypto (native module; not loadable under Node).
// Wired up via the alias in vitest.config.ts.
import { randomFillSync } from "node:crypto";

export function getRandomBytes(byteCount: number): Uint8Array {
  const bytes = new Uint8Array(byteCount);
  randomFillSync(bytes);
  return bytes;
}
