import type { FileDownloadProgress } from "../types";

export const WS_HANDSHAKE = "tidebreak-v1";

export const WS_TOKEN_PREFIX = "tidebreak-token.";

const DEFAULT_DELIVERY_TIMEOUT_MS = 30_000;

export type DeliveryRequestOptions = {
  signal?: AbortSignal;
  timeoutMs?: number;
  refreshAuth?: boolean;
};

/**
 * The size below which a transfer is not worth reporting on.
 *
 * A bar that appears and vanishes is worse than no bar, and a source under this
 * arrives in a couple of chunks — most of them from a sidecar on this machine.
 * Well under the 16 MB a source may be, so the files big enough to wait on do
 * still report.
 */
const PROGRESS_MIN_BYTES = 2 * 1024 * 1024;

/** Progress updates are worth at most one re-render per frame budget. */
const PROGRESS_THROTTLE_MS = 100;

/**
 * Rate-limit progress callbacks, always letting the last one through.
 *
 * Without the trailing call the bar can stop short of the end: the final chunk
 * usually lands inside the throttle window of the one before it.
 */
function throttle(
  report: (progress: FileDownloadProgress) => void,
): (progress: FileDownloadProgress) => void {
  let last = 0;
  return (progress) => {
    const now = Date.now();
    if (
      progress.loaded >= progress.total ||
      now - last >= PROGRESS_THROTTLE_MS
    ) {
      last = now;
      report(progress);
    }
  };
}

/**
 * A rejected response, carrying the status so a caller can tell why.
 *
 * The status is what separates "this is gone" from "we could not reach the
 * server": a panel that cannot tell those apart has to guess, and guessing
 * wrong tells a reader their file was deleted when it was not.
 */
export class HttpError extends Error {
  constructor(
    readonly status: number,
    message: string,
    readonly kind?: string,
  ) {
    super(message);
    this.name = "HttpError";
  }
}

/** Archive is refused until the reader confirms discarding leftover work. */
export const ARCHIVE_FORCE_KINDS = new Set([
  "uncommitted",
  "unpushed",
  "uncommitted_and_unpushed",
  "ignored_content",
]);

/** The 409 kinds that mean archive needs an explicit force. */
export function archiveForceKind(error: unknown): string | null {
  if (!(error instanceof HttpError) || !error.kind) return null;
  return ARCHIVE_FORCE_KINDS.has(error.kind) ? error.kind : null;
}

/** The server's own message for a failed response, or its status text. */
export async function throwIfNotOk(response: Response): Promise<void> {
  if (response.ok) return;
  let detail = response.statusText;
  let kind: string | undefined;
  try {
    const body = (await response.json()) as { message?: string; kind?: string };
    if (body.message) detail = body.message;
    if (typeof body.kind === "string" && body.kind.length > 0) kind = body.kind;
  } catch {
    /* ignore */
  }
  throw new HttpError(response.status, `${response.status}: ${detail}`, kind);
}

export function requireParsed<T>(value: T | null, label: string): T {
  if (!value) throw new Error(`${label} response contains invalid data`);
  return value;
}

export function parseList<T>(
  body: unknown,
  parse: (value: unknown) => T | null,
  label: string,
): T[] {
  if (!Array.isArray(body)) {
    throw new Error(`${label} response is not an array`);
  }
  return body.map((item, index) => {
    const parsed = parse(item);
    if (!parsed) {
      throw new Error(`${label} response contains invalid data at ${index}`);
    }
    return parsed;
  });
}

export type Constructor<T = object> = new (...args: any[]) => T;

/**
 * Bearer auth, JSON and byte transport, and the delivery timeout wrapper.
 *
 * Every facet under `./client/` extends this class through a mixin; the
 * composed `ApiClient` in `../client.ts` is what the app constructs.
 */
export class HttpCore {
  protected accessToken: string;

  constructor(
    readonly baseUrl: string,
    token: string,
  ) {
    this.accessToken = token;
  }

  get token(): string {
    return this.accessToken;
  }

  /** Replace the short-lived bearer used by subsequent HTTP and WebSocket connections. */
  setAccessToken(token: string): void {
    this.accessToken = token;
  }

  protected headers(json = false): HeadersInit {
    const headers: Record<string, string> = {
      Authorization: `Bearer ${this.token}`,
    };
    if (json) headers["Content-Type"] = "application/json";
    return headers;
  }

  protected async json<T>(
    path: string,
    init?: RequestInit,
    expectedStatus?: number,
  ): Promise<T> {
    const response = await fetch(`${this.baseUrl}${path}`, init);
    await throwIfNotOk(response);
    if (expectedStatus !== undefined && response.status !== expectedStatus) {
      throw new Error(
        `unexpected response status: expected ${expectedStatus}, received ${response.status}`,
      );
    }
    if (response.status === 204) return undefined as T;
    const text = await response.text();
    if (text.length === 0) return undefined as T;
    return JSON.parse(text) as T;
  }

  protected async deliveryJson<T>(
    path: string,
    init: RequestInit,
    options: DeliveryRequestOptions = {},
  ): Promise<T> {
    const controller = new AbortController();
    const timeoutMs = options.timeoutMs ?? DEFAULT_DELIVERY_TIMEOUT_MS;
    let timedOut = false;
    const abortFromCaller = () => controller.abort(options.signal?.reason);
    if (options.signal?.aborted) abortFromCaller();
    else
      options.signal?.addEventListener("abort", abortFromCaller, {
        once: true,
      });
    const timeout = globalThis.setTimeout(() => {
      timedOut = true;
      controller.abort();
    }, timeoutMs);
    try {
      return await this.json<T>(path, { ...init, signal: controller.signal });
    } catch (error) {
      if (timedOut) {
        throw new Error(
          `GitHub delivery request timed out after ${Math.ceil(timeoutMs / 1_000)} seconds.`,
        );
      }
      throw error;
    } finally {
      globalThis.clearTimeout(timeout);
      options.signal?.removeEventListener("abort", abortFromCaller);
    }
  }

  protected async blob(path: string, signal?: AbortSignal): Promise<Blob> {
    const response = await fetch(`${this.baseUrl}${path}`, {
      headers: this.headers(),
      signal,
    });
    await throwIfNotOk(response);
    return response.blob();
  }

  /**
   * Bytes read as they arrive, reporting how much has landed.
   *
   * Reading the body as a stream rather than awaiting it whole is the only way
   * to say anything about a transfer while it is still running. It costs an
   * extra copy — the chunks are joined once at the end — which is why the
   * callers that have nothing to report progress to still use {@link blob}.
   *
   * `onProgress` is only ever called when the response declares its length:
   * without a total there is no fraction to report, and a byte count climbing
   * toward an unknown end is not worth a progress bar.
   */
  protected async streamBytes(
    path: string,
    signal?: AbortSignal,
    onProgress?: (progress: FileDownloadProgress) => void,
  ): Promise<{ bytes: Uint8Array; contentType: string | null }> {
    const response = await fetch(`${this.baseUrl}${path}`, {
      headers: this.headers(),
      signal,
    });
    await throwIfNotOk(response);

    const contentType = response.headers.get("Content-Type");
    const declared = Number(response.headers.get("Content-Length"));
    const total = Number.isSafeInteger(declared) && declared > 0 ? declared : 0;

    // No reader to stream from (an old runtime, or a mocked response in a
    // test): take the whole body and skip straight to the finished state.
    if (!response.body) {
      const bytes = new Uint8Array(await response.arrayBuffer());
      return { bytes, contentType };
    }

    const report =
      onProgress && total > PROGRESS_MIN_BYTES ? throttle(onProgress) : null;
    const reader = response.body.getReader();
    const chunks: Uint8Array[] = [];
    let loaded = 0;

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(value);
      loaded += value.length;
      report?.({ loaded, total, percentage: (loaded / total) * 100 });
    }

    const bytes = new Uint8Array(loaded);
    let offset = 0;
    for (const chunk of chunks) {
      bytes.set(chunk, offset);
      offset += chunk.length;
    }
    return { bytes, contentType };
  }
}
