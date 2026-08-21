import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/**
 * A caught value as something worth showing a reader.
 *
 * The backend returns human-readable strings, so the caught message is usually
 * the best copy available. Anything empty or long enough to be a stack trace or
 * a serialized payload falls back to the caller's own wording.
 */
export function friendlyErrorMessage(error: unknown, fallback: string): string {
  const message = String(error)
    .replace(/^Error:\s*/, "")
    .trim();
  return message && message.length <= 240 ? message : fallback;
}
