/**
 * The HTTP client for one gateway resource.
 *
 * Three things separate it from `MachineClient` (`machine.ts`), which talks to
 * a Tidebreak machine rather than to the gateway itself:
 *
 * **Two refusal shapes, normalized.** The versioned control-plane surfaces
 * nest their error — `{ error: { code, message } }` — while the runtime
 * surface is OAuth-shaped and flat: `{ error: "<code>", error_description }`
 * (`RuntimeHttpError`). Both reach this client, and a caller that branches on
 * a code (`sandboxes_not_enabled`, `revision_moved`, `sandbox_already_terminal`)
 * must get the same `code` field from either. Collapsing them here is the
 * whole reason this class exists rather than a bare fetch per screen.
 *
 * **Redirects are refused.** Every request carries a bearer token minted for
 * one resource; following a `Location` would hand it to whatever host the
 * header names. `fetchRefusingRedirects` is the repo's answer (`http.ts`) and
 * is used unchanged.
 *
 * **The resource is the client's identity.** The gateway mints one access
 * token per resource, so `control`, `control_plane`, and `runtime:<slug>` are
 * three clients over one base URL and one refresh family — never one client
 * with a swappable audience.
 */

import { fetchRefusingRedirects, type HttpFetch, type HttpRequestInit } from "./http";

/** A non-OK answer from the gateway, with whichever code it carried. */
export class HttpError extends Error {
  readonly status: number;
  readonly path: string;
  readonly code: string | undefined;

  constructor(message: string, status: number, path: string, code?: string) {
    super(message);
    this.name = "HttpError";
    this.status = status;
    this.path = path;
    this.code = code;
  }
}

/** What the client needs to mint a bearer for its resource. */
export type GatewayTokenSource = {
  getAccessToken: (resource: string) => Promise<string>;
};

export type GatewayClientOptions = {
  baseUrl: string;
  /** The gateway resource these requests authenticate against. */
  resource: string;
  tokens: GatewayTokenSource;
  fetchImpl?: HttpFetch;
};

export type GatewayRequestOptions = {
  method?: string;
  body?: unknown;
  signal?: AbortSignal;
};

/**
 * The code and message a refusal carried, from either wire shape.
 *
 * Exported for its own test: this is the one piece of the client whose
 * behaviour a screen depends on without going through the network.
 */
export function parseRefusal(
  json: unknown,
  fallbackMessage: string,
): { message: string; code: string | undefined } {
  if (!json || typeof json !== "object") {
    return { message: fallbackMessage, code: undefined };
  }
  const body = json as { error?: unknown; error_description?: unknown };
  const error = body.error;
  // Flat, OAuth-shaped: the runtime surface.
  if (typeof error === "string") {
    const description = body.error_description;
    return {
      message:
        typeof description === "string" && description.length > 0
          ? description
          : fallbackMessage,
      code: error.length > 0 ? error : undefined,
    };
  }
  // Nested: the versioned control-plane surfaces.
  if (error && typeof error === "object") {
    const nested = error as { code?: unknown; message?: unknown };
    return {
      message:
        typeof nested.message === "string" && nested.message.length > 0
          ? nested.message
          : fallbackMessage,
      code: typeof nested.code === "string" ? nested.code : undefined,
    };
  }
  return { message: fallbackMessage, code: undefined };
}

export class GatewayClient {
  private readonly baseUrl: string;
  readonly resource: string;

  constructor(private readonly options: GatewayClientOptions) {
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.resource = options.resource;
  }

  async request<T>(
    path: string,
    request: GatewayRequestOptions = {},
  ): Promise<T> {
    const token = await this.options.tokens.getAccessToken(this.resource);
    const headers: Record<string, string> = {
      Accept: "application/json",
      Authorization: `Bearer ${token}`,
    };
    const init: HttpRequestInit = { headers };
    if (request.method) {
      init.method = request.method;
    }
    if (request.body !== undefined) {
      headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(request.body);
    }
    if (request.signal) {
      init.signal = request.signal;
    }
    const response = await fetchRefusingRedirects(
      `${this.baseUrl}${path}`,
      init,
      this.options.fetchImpl,
    );
    if (!response.ok) {
      let json: unknown = null;
      try {
        json = await response.json();
      } catch {
        // A non-JSON body (an HTML proxy page) says nothing worth reflecting.
        json = null;
      }
      const { message, code } = parseRefusal(
        json,
        `Gateway request failed (HTTP ${response.status})`,
      );
      throw new HttpError(message, response.status, path, code);
    }
    if (response.status === 204) {
      return undefined as T;
    }
    return (await response.json()) as T;
  }
}

/** Whether a failure is the gateway answering with one particular code. */
export function hasErrorCode(error: unknown, code: string): boolean {
  return error instanceof HttpError && error.code === code;
}
