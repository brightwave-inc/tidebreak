import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

import { HttpError } from "../api/client/http";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/**
 * A caught value as something worth showing a reader.
 *
 * Prefer `Error.message` over `String(error)` so `HttpError` (whose `name` is
 * `"HttpError"`) does not leak a `"HttpError: "` prefix. The HTTP client
 * prefixes the status (`"409: …"`); strip that when the body already carries
 * the server's `message`. Server detail is already bounded (git stderr up to
 * 4 KB); keep the 240-character cap only for unknown shapes so a toast stays
 * readable.
 */
export function friendlyErrorMessage(error: unknown, fallback: string): string {
  let message = (error instanceof Error ? error.message : String(error))
    .replace(/^Error:\s*/, "")
    .trim();
  if (error instanceof HttpError) {
    // The client builds the message as `${status}: ${server message}`, so
    // strip exactly that status, never digits the server itself wrote.
    const prefix = `${error.status}:`;
    if (message.startsWith(prefix)) {
      message = message.slice(prefix.length).trim();
    }
    return message || fallback;
  }
  return message && message.length <= 240 ? message : fallback;
}
