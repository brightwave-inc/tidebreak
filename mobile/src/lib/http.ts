/**
 * Fetch that refuses HTTP redirects.
 *
 * Attach, token, and machine calls carry a bearer token, a PKCE code, or the
 * rotating refresh token. Following a redirect would hand that credential to
 * whatever host the `Location` header names, so the desktop's HTTP client
 * disables redirects outright. Mirror it: ask the runtime not to follow
 * (`redirect: "manual"`), then refuse any response that is a redirect — or
 * that reports one was followed anyway, because React Native's XHR-backed
 * fetch can follow at the native layer before this code runs.
 */
export async function fetchRefusingRedirects(
  fetchImpl: typeof fetch,
  url: string,
  init?: RequestInit,
): Promise<Response> {
  const response = await fetchImpl(url, { ...init, redirect: "manual" });
  if (isRedirect(response, url)) {
    throw new Error(
      "The server answered with a redirect; refusing to follow it.",
    );
  }
  return response;
}

function isRedirect(response: Response, requestedUrl: string): boolean {
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
