/**
 * Fetch that refuses HTTP redirects.
 *
 * Attach, token, and machine calls carry a bearer token, a PKCE code, or the
 * rotating refresh token. Following a redirect would hand that credential to
 * whatever host the `Location` header names, so the desktop's HTTP client
 * disables redirects outright. React Native's global `fetch` cannot mirror
 * that: it is whatwg-fetch over XHR, and the native layer (NSURLSession,
 * OkHttp) follows the redirect — re-sending the body and headers — before JS
 * ever sees the response. The default transport here is therefore
 * `expo/fetch`, whose native module turns redirects off when the mode is not
 * `"follow"` (OkHttp `followRedirects(false)`; URLSession's
 * `willPerformHTTPRedirection` completes with nil), so a 3xx comes back
 * un-followed. The response inspection stays as defense in depth for any
 * injected transport that ignores the requested mode.
 */

/** The request surface these calls need; a subset of `RequestInit`. */
export type HttpRequestInit = {
  method?: string;
  headers?: Record<string, string>;
  body?: string;
  signal?: AbortSignal;
};

/** The response surface shared by DOM `Response` and expo's `FetchResponse`. */
export type HttpResponse = {
  readonly status: number;
  readonly ok: boolean;
  readonly redirected: boolean;
  readonly url: string;
  readonly type?: string;
  json(): Promise<unknown>;
  text(): Promise<string>;
};

export type HttpFetch = (
  url: string,
  init: HttpRequestInit & { redirect: "manual" },
) => Promise<HttpResponse>;

let nativeTransport: HttpFetch | null = null;

async function loadNativeTransport(): Promise<HttpFetch> {
  if (!nativeTransport) {
    // Lazy so unit tests that always inject a transport never touch the
    // native module.
    const module = await import("expo/fetch");
    nativeTransport = module.fetch as unknown as HttpFetch;
  }
  return nativeTransport;
}

export async function fetchRefusingRedirects(
  url: string,
  init?: HttpRequestInit,
  fetchImpl?: HttpFetch,
): Promise<HttpResponse> {
  const transport = fetchImpl ?? (await loadNativeTransport());
  const response = await transport(url, { ...init, redirect: "manual" });
  if (isRedirect(response, url)) {
    throw new Error(
      "The server answered with a redirect; refusing to follow it.",
    );
  }
  return response;
}

function isRedirect(response: HttpResponse, requestedUrl: string): boolean {
  if (response.type === "opaqueredirect") {
    return true;
  }
  if (response.status >= 300 && response.status < 400) {
    return true;
  }
  if (response.redirected) {
    return true;
  }
  if (response.url) {
    try {
      const landed = new URL(response.url);
      const asked = new URL(requestedUrl);
      return landed.origin !== asked.origin || landed.pathname !== asked.pathname;
    } catch {
      return false;
    }
  }
  return false;
}
