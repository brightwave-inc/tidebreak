/**
 * Report errors the renderer could not handle to this machine's own log.
 *
 * Three places catch them: the React error boundary, the window's `error`
 * event, and its `unhandledrejection` event. Each report goes to
 * `POST /diagnostics/renderer-errors`, which writes it to the embedded
 * server's human log, where a diagnostics export picks it up.
 *
 * Reports stay on this computer. Nothing is sent while the window works on a
 * remote machine, and a report never goes anywhere else. The reporter keeps
 * its own limits on top of the server's: a bounded number per page load, and
 * one report for an error that repeats.
 *
 * A report follows the log's rules before it leaves the window: no URL query
 * strings, no credentials or tokens, and no values a rejection carried, so a
 * prompt inside a rejected object never reaches the log. The server applies
 * the same scrub again when it writes the line.
 */

export type RendererErrorKind = "render" | "error" | "unhandled_rejection";

/** The body `POST /diagnostics/renderer-errors` accepts. */
type RendererErrorReport = {
  kind: RendererErrorKind;
  message: string;
  stack?: string;
  component_stack?: string;
  source?: string;
  line?: number;
  column?: number;
};

export type RendererErrorDetail = {
  componentStack?: string | null;
  source?: string;
  line?: number;
  column?: number;
};

/** Where reports go: the machine the window is attached to, when local. */
export type RendererErrorConnection = {
  baseUrl: string;
  token: string;
  attachment: "local" | "remote";
};

const REPORT_PATH = "/diagnostics/renderer-errors";
/** The most reports one page load sends. */
const MAX_REPORTS_PER_PAGE = 50;
/** Reports held until the connection is known. */
const MAX_QUEUED_REPORTS = 10;
/** The same error again within this long is not sent again. */
const REPEAT_WINDOW_MS = 60_000;
const MAX_MESSAGE_CHARS = 1_000;
const MAX_STACK_CHARS = 8_000;
const MAX_SOURCE_CHARS = 500;
/** The most keys a report names for a rejected object. */
const MAX_DESCRIBED_KEYS = 10;
const MAX_KEY_CHARS = 40;

type ReporterOptions = {
  fetch?: typeof globalThis.fetch;
  now?: () => number;
};

export type RendererErrorReporter = {
  /** Point reports at the attached machine, or stop sending with `null`. */
  connect: (connection: RendererErrorConnection | null) => void;
  report: (
    kind: RendererErrorKind,
    error: unknown,
    detail?: RendererErrorDetail,
  ) => void;
  /** Listen for the window's uncaught errors. Returns the unsubscribe. */
  install: (target: Window) => () => void;
};

export function createRendererErrorReporter({
  fetch = (...args) => globalThis.fetch(...args),
  now = () => Date.now(),
}: ReporterOptions = {}): RendererErrorReporter {
  let connection: { baseUrl: string; token: string } | null = null;
  let connected = false;
  let queued: RendererErrorReport[] = [];
  let sent = 0;
  const lastSeen = new Map<string, number>();

  const send = (report: RendererErrorReport) => {
    if (!connection || sent >= MAX_REPORTS_PER_PAGE) return;
    sent += 1;
    const { baseUrl, token } = connection;
    const body = JSON.stringify(redact(report, token));
    try {
      void fetch(`${baseUrl}${REPORT_PATH}`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body,
        keepalive: true,
      }).catch(() => undefined);
    } catch {
      // Reporting must never become the next error.
    }
  };

  const report: RendererErrorReporter["report"] = (kind, error, detail) => {
    const built = buildReport(kind, error, detail);
    const key = `${kind}\u0000${built.message}\u0000${firstFrame(built.stack)}`;
    const at = now();
    const previous = lastSeen.get(key);
    if (previous !== undefined && at - previous < REPEAT_WINDOW_MS) return;
    lastSeen.set(key, at);
    if (!connected) {
      if (queued.length < MAX_QUEUED_REPORTS) queued.push(built);
      return;
    }
    send(built);
  };

  return {
    connect(next) {
      connected = true;
      connection =
        next && next.attachment === "local"
          ? { baseUrl: next.baseUrl.replace(/\/+$/, ""), token: next.token }
          : null;
      const pending = queued;
      queued = [];
      for (const held of pending) send(held);
    },
    report,
    install(target) {
      const onError = (event: ErrorEvent) => {
        report("error", event.error ?? event.message, {
          source: event.filename || undefined,
          line: event.lineno || undefined,
          column: event.colno || undefined,
        });
      };
      const onRejection = (event: PromiseRejectionEvent) => {
        report("unhandled_rejection", event.reason);
      };
      target.addEventListener("error", onError);
      target.addEventListener("unhandledrejection", onRejection);
      return () => {
        target.removeEventListener("error", onError);
        target.removeEventListener("unhandledrejection", onRejection);
      };
    },
  };
}

function buildReport(
  kind: RendererErrorKind,
  error: unknown,
  detail: RendererErrorDetail = {},
): RendererErrorReport {
  const report: RendererErrorReport = {
    kind,
    message: scrubLogText(errorMessage(error), MAX_MESSAGE_CHARS),
  };
  const stack = error instanceof Error ? error.stack : undefined;
  if (stack) report.stack = scrubLogText(stack, MAX_STACK_CHARS);
  if (detail.componentStack) {
    report.component_stack = scrubLogText(
      detail.componentStack,
      MAX_STACK_CHARS,
    );
  }
  if (detail.source) {
    report.source = scrubLogText(detail.source, MAX_SOURCE_CHARS);
  }
  if (isCount(detail.line)) report.line = detail.line;
  if (isCount(detail.column)) report.column = detail.column;
  return report;
}

/**
 * What a report says about a thrown or rejected value. An error's own message
 * is kept, scrubbed later; any other object is named by its kind and its keys,
 * never its values.
 */
function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message ? `${error.name}: ${error.message}` : error.name;
  }
  if (typeof error === "string") return error || "An empty error";
  if (typeof error === "function") return "Function (not an Error)";
  if (typeof error === "symbol") return "Symbol (not an Error)";
  // raw-error-ok: a log line, scrubbed before it is written, never shown.
  if (typeof error !== "object" || error === null) return String(error);
  try {
    const { message, name } = error as { message?: unknown; name?: unknown };
    if (typeof message === "string" && message) {
      return typeof name === "string" && name ? `${name}: ${message}` : message;
    }
  } catch {
    // A getter or a proxy threw; describe the object instead.
  }
  return describeObject(error);
}

/** A rejected object by its kind and its keys, such as `Object with keys a, b`. */
function describeObject(value: object): string {
  if (Array.isArray(value)) {
    return `Array of ${value.length} items (not an Error)`;
  }
  let kind = "Object";
  try {
    const constructor = Object.getPrototypeOf(value)?.constructor as
      | { name?: unknown }
      | undefined;
    if (typeof constructor?.name === "string" && constructor.name) {
      kind = constructor.name;
    }
  } catch {
    // A proxy can throw here; the plain word will do.
  }
  let keys: string[] = [];
  try {
    keys = Object.keys(value);
  } catch {
    // As above.
  }
  if (keys.length === 0) return `${kind} (not an Error)`;
  const named = keys
    .slice(0, MAX_DESCRIBED_KEYS)
    .map((key) => clip(key, MAX_KEY_CHARS))
    .join(", ");
  const more = keys.length > MAX_DESCRIBED_KEYS ? ", …" : "";
  return `${kind} with keys ${named}${more} (not an Error)`;
}

function isCount(value: number | undefined): value is number {
  return (
    value !== undefined &&
    Number.isInteger(value) &&
    value > 0 &&
    value <= 0xffff_ffff
  );
}

function clip(text: string, limit: number): string {
  return text.length > limit ? `${text.slice(0, limit)}…` : text;
}

function firstFrame(stack: string | undefined): string {
  return stack?.split("\n", 2)[1]?.trim() ?? "";
}

// The scrub below follows the server's `logging::scrub_log_text` rule for
// rule, so a line reads the same whichever side scrubbed it. The tests hold
// both to the same cases.

const REDACTED = "[redacted]";

/** Token prefixes that mark a credential whatever surrounds them. */
const SECRET_PREFIXES = [
  "sk-",
  "sk_live_",
  "sk_test_",
  "rk_live_",
  "pk_live_",
  "ghp_",
  "gho_",
  "ghu_",
  "ghs_",
  "ghr_",
  "github_pat_",
  "glpat-",
  "xoxb-",
  "xoxp-",
  "xoxa-",
  "xoxr-",
  "xapp-",
  "AKIA",
  "ASIA",
  "AIza",
  "ya29.",
  "npm_",
  "hf_",
  "tbreak_",
  "tidebreak-token.",
];

/** Characters past a prefix before a word counts as a credential. */
const MIN_SECRET_TAIL = 8;

/** Key names whose value is a credential, matched inside the key. */
const SECRET_KEYS = [
  "token",
  "secret",
  "password",
  "passwd",
  "apikey",
  "api_key",
  "api-key",
  "authorization",
  "cookie",
  "credential",
];

/** Authorization schemes kept after a credential key. */
const AUTH_SCHEMES = ["bearer", "basic", "token", "digest"];

/** Rust's `char::is_whitespace`: Unicode `White_Space`. */
const WHITESPACE =
  /[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]/u;
const NOT_WHITESPACE =
  /[^\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]/u;
/** Rust's `char::is_alphanumeric`. */
const ALPHANUMERIC = /^[\p{Alphabetic}\p{N}]$/u;
const SECRET_CHARACTER = /[A-Za-z0-9_.-]/;
const WRAPPING = new Set([
  '"',
  "'",
  "`",
  "(",
  ")",
  "[",
  "]",
  "{",
  "}",
  "<",
  ">",
  ",",
  ";",
]);
const URL_CLOSERS = new Set([")", "]", "}", '"', "'", ">", ",", ";"]);

/**
 * Make free-form text fit for the log, then cut it to `limit` characters: no
 * URL query strings, fragments, or userinfo, no value after a credential key
 * or an authorization scheme, and no vendor, Tidebreak, or web tokens.
 */
export function scrubLogText(text: string, limit: number): string {
  const bound = Math.max(limit * 4, 1024);
  const bounded = Array.from(text.slice(0, bound * 2))
    .slice(0, bound)
    .join("");
  let scrubbed = "";
  let redactNext = false;
  let rest = bounded;
  while (rest.length > 0) {
    const space = firstIndex(rest, NOT_WHITESPACE);
    scrubbed += rest.slice(0, space);
    rest = rest.slice(space);
    if (rest.length === 0) break;
    const end = firstIndex(rest, WHITESPACE);
    const word = rest.slice(0, end);
    rest = rest.slice(end);
    if (redactNext) {
      if (AUTH_SCHEMES.includes(asciiLower(trimWord(word)))) {
        scrubbed += word;
      } else {
        scrubbed += REDACTED;
        redactNext = false;
      }
      continue;
    }
    const [kept, next] = scrubWord(word);
    scrubbed += kept;
    redactNext = next;
  }
  const characters = Array.from(scrubbed);
  return characters.length > limit
    ? `${characters.slice(0, limit).join("")}…`
    : scrubbed;
}

function firstIndex(text: string, pattern: RegExp): number {
  const found = text.search(pattern);
  return found === -1 ? text.length : found;
}

function scrubWord(word: string): [string, boolean] {
  if (asciiLower(trimWord(word)) === "bearer") return [word, true];
  const scheme = word.indexOf("://");
  if (scheme !== -1) return [scrubUrl(word, scheme + 3), false];
  const query = word.indexOf("?");
  if (query !== -1 && word.slice(query).includes("=")) {
    return [`${word.slice(0, query)}?${REDACTED}`, false];
  }
  const split = word.search(/[=:]/);
  if (split !== -1) {
    const key = asciiLower(trimWord(word.slice(0, split)));
    if (SECRET_KEYS.some((secret) => key.includes(secret))) {
      if (trimWord(word.slice(split + 1)) === "") return [word, true];
      return [`${word.slice(0, split + 1)}${REDACTED}`, false];
    }
  }
  return [redactTokens(word), false];
}

function scrubUrl(word: string, authority: number): string {
  const head = word.slice(0, authority);
  const tail = word.slice(authority);
  const authorityEnd = firstIndex(tail, /[/?#]/);
  let host = tail.slice(0, authorityEnd);
  let path = tail.slice(authorityEnd);
  const at = host.lastIndexOf("@");
  if (at !== -1) host = `${REDACTED}@${host.slice(at + 1)}`;
  const cut = path.search(/[?#]/);
  if (cut !== -1) {
    let closing = path.length;
    while (closing > 0 && URL_CLOSERS.has(path[closing - 1] ?? "")) {
      closing -= 1;
    }
    closing = Math.max(closing, cut + 1);
    path = `${path.slice(0, cut + 1)}${REDACTED}${path.slice(closing)}`;
  }
  return `${head}${host}${path}`;
}

function redactTokens(word: string): string {
  let out = "";
  let index = 0;
  let afterAlphanumeric = false;
  while (index < word.length) {
    if (!afterAlphanumeric) {
      const length = secretAt(word.slice(index));
      if (length !== null) {
        out += REDACTED;
        index += length;
        afterAlphanumeric = true;
        continue;
      }
    }
    const character = String.fromCodePoint(word.codePointAt(index) ?? 0);
    out += character;
    afterAlphanumeric = ALPHANUMERIC.test(character);
    index += character.length;
  }
  return out;
}

function secretAt(text: string): number | null {
  let run = 0;
  while (run < text.length && SECRET_CHARACTER.test(text[run] ?? "")) run += 1;
  const token = text.slice(0, run);
  const prefixed = SECRET_PREFIXES.some(
    (prefix) =>
      token.startsWith(prefix) &&
      token.length >= prefix.length + MIN_SECRET_TAIL,
  );
  const webToken = token.startsWith("eyJ") && token.split(".").length - 1 >= 2;
  return prefixed || webToken ? run : null;
}

function trimWord(word: string): string {
  let start = 0;
  let end = word.length;
  while (start < end && WRAPPING.has(word[start] ?? "")) start += 1;
  while (end > start && WRAPPING.has(word[end - 1] ?? "")) end -= 1;
  return word.slice(start, end);
}

function asciiLower(text: string): string {
  return text.replace(/[A-Z]/g, (letter) => letter.toLowerCase());
}

/** Keep the launch bearer out of the log, should an error message carry it. */
function redact(
  report: RendererErrorReport,
  token: string,
): RendererErrorReport {
  if (!token) return report;
  const scrub = (text: string | undefined) =>
    text?.split(token).join("[token]");
  return {
    ...report,
    message: scrub(report.message) ?? report.message,
    stack: scrub(report.stack),
    component_stack: scrub(report.component_stack),
    source: scrub(report.source),
  };
}

/** The app's one reporter. */
export const rendererErrors = createRendererErrorReporter();

export function reportRendererError(
  kind: RendererErrorKind,
  error: unknown,
  detail?: RendererErrorDetail,
): void {
  rendererErrors.report(kind, error, detail);
}
