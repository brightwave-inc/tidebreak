import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

import { HttpError } from "../api/client/http";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** What a reader sees when a request never reached Tidebreak's server. */
export const UNREACHABLE_SERVER_MESSAGE =
  "Tidebreak could not reach its server. Check that the app is running, then try again.";

/** A request the server accepted but did not answer in time. */
export const SERVER_TIMEOUT_MESSAGE =
  "Tidebreak's server took too long to answer. Try again in a moment.";

/** A 5xx whose body was not the server's own error (a proxy, a restart). */
export const SERVER_UNAVAILABLE_MESSAGE =
  "Tidebreak's server is not answering right now. Try again in a moment.";

/**
 * Renderer copy for the server error kinds whose text is never written for a
 * reader. Every other kind keeps the server's message, because that message
 * is usually the one detail the reader can act on ("Sign in to save your
 * subscription preference.", "No pull request exists for this branch.").
 *
 * Each entry is checked against the server: `store` wraps a persistence
 * failure ("store error: …"), `serde` a JSON parse failure, and `secret` a
 * keychain backend failure, which the server itself redacts wherever an
 * operator cannot fix it. A caller that knows more passes its own copy.
 */
export const ERROR_KIND_COPY: Readonly<Record<string, string>> = {
  store:
    "Tidebreak could not read its local data. Try again, and restart the app if it keeps happening.",
  serde:
    "Tidebreak could not read part of its data. Try again, and restart the app if it keeps happening.",
  secret:
    "Tidebreak could not read its saved credentials. Check that your keychain is unlocked, then try again.",
};

/**
 * What `fetch` rejects with when the request never got an answer: WebKit's
 * "Load failed", Chromium's "Failed to fetch", Firefox's "NetworkError…".
 */
const FETCH_FAILURE =
  /^(?:load failed|failed to fetch|networkerror when attempting to fetch resource\.?|the network connection was lost\.?|network request failed|could not connect to the server\.?)$/i;

/**
 * A reason phrase alone: what the client falls back to when a response has
 * no JSON error body, so it says nothing the status did not.
 */
const STATUS_TEXT =
  /^(?:bad request|unauthorized|payment required|forbidden|not found|method not allowed|not acceptable|request timeout|conflict|gone|(?:payload|content) too large|unsupported media type|unprocessable (?:entity|content)|too many requests|internal server error|not implemented|bad gateway|service unavailable|gateway timeout)$/i;

/** `Error: `, `TypeError: `, `HttpError: ` — the class name `String(err)` adds. */
const ERROR_NAME_PREFIX = /^(?:[A-Z][A-Za-z]*)?Error:\s*/;

/** `HttpError: 409: …`, the way `String(err)` spells an `HttpError`. */
const STRINGIFIED_HTTP_ERROR = /^HttpError:\s*(\d{3}):\s*([\s\S]*)$/;

/**
 * A caught value as something worth showing a reader.
 *
 * The one formatter for every failure the UI shows. It never lets an
 * exception's class name, a status code, or a browser's network jargon
 * through:
 *
 * - A request that never reached the server reads as
 *   {@link UNREACHABLE_SERVER_MESSAGE}.
 * - An `HttpError` reads as the server's own message, started as a sentence
 *   and with the client's `"409: "` status prefix stripped. Server detail is
 *   already bounded (git stderr up to 4 KB), so it is not capped. The
 *   caller's `kindCopy` wins for the kinds it names, then
 *   {@link ERROR_KIND_COPY} for the few kinds whose text is never for a
 *   reader.
 * - A response with nothing but its status (no message, or only the reason
 *   phrase a proxy wrote) reads as {@link SERVER_UNAVAILABLE_MESSAGE} for a
 *   5xx and as `fallback` otherwise, whether it arrives as an `HttpError` or
 *   as the string `String(err)` made of one.
 * - Anything else reads as its message without the `Error:` prefix, capped
 *   at 240 characters so a toast stays readable; past that, or when there is
 *   no message at all, the reader gets `fallback`.
 */
export function friendlyErrorMessage(
  error: unknown,
  fallback: string,
  kindCopy?: Readonly<Record<string, string>>,
): string {
  if (error instanceof HttpError) {
    const copy =
      (error.kind && (kindCopy?.[error.kind] ?? ERROR_KIND_COPY[error.kind])) ||
      null;
    if (copy) return copy;
    // Tidebreak's server names a kind on every error it answers with, so a
    // 5xx without one came from whatever stood in front of it: a proxy, or
    // a server that is restarting.
    if (!error.kind && error.status >= 500) return SERVER_UNAVAILABLE_MESSAGE;
    // The client builds the message as `${status}: ${server message}`, so
    // strip exactly that status, never digits the server itself wrote.
    let message = error.message.trim();
    const prefix = `${error.status}:`;
    if (message.startsWith(prefix)) {
      message = message.slice(prefix.length).trim();
    }
    return serverMessage(error.status, message, fallback);
  }
  if (isTimeout(error)) return SERVER_TIMEOUT_MESSAGE;
  const raw =
    error instanceof Error
      ? error.message
      : typeof error === "string"
        ? error
        : error == null
          ? ""
          : // raw-error-ok: the formatter itself reads an unknown value.
            String(error);
  const named = raw.trim();
  // A stringified `HttpError` keeps its status; read it the way the error
  // itself would have been read.
  const stringified = STRINGIFIED_HTTP_ERROR.exec(named);
  if (stringified) {
    return serverMessage(
      Number(stringified[1]),
      stringified[2].trim(),
      fallback,
    );
  }
  const message = named.replace(ERROR_NAME_PREFIX, "").trim();
  if (FETCH_FAILURE.test(message)) return UNREACHABLE_SERVER_MESSAGE;
  if (STATUS_TEXT.test(message)) return fallback;
  return message && message.length <= 240 ? message : fallback;
}

/**
 * What the server said, once the status is off it: its own message as a
 * sentence, or, when it said nothing a reader can use, the copy for its
 * status.
 */
function serverMessage(
  status: number,
  message: string,
  fallback: string,
): string {
  if (!message || STATUS_TEXT.test(message)) {
    return status >= 500 ? SERVER_UNAVAILABLE_MESSAGE : fallback;
  }
  return sentenceStart(message);
}

/** Names that stay lowercase at the start of a sentence. */
const LOWERCASE_NAMES = new Set([
  "gh",
  "npm",
  "npx",
  "pip",
  "pnpm",
  "uv",
  "yarn",
]);

/**
 * The server writes log-style fragments ("app not found"); the screen speaks
 * in sentences. Capitalize a plain first word, and leave anything that looks
 * like an identifier (`api_key`, `git:`, `iOS`) as the server wrote it.
 */
export function sentenceStart(message: string): string {
  const first = /^([a-z]+)(?=\s)/.exec(message)?.[1];
  if (!first || LOWERCASE_NAMES.has(first)) return message;
  return message[0].toUpperCase() + message.slice(1);
}

function isTimeout(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    "name" in error &&
    error.name === "TimeoutError"
  );
}
