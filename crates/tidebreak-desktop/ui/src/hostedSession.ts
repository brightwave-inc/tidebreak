/**
 * A browser tab served by the machine itself.
 *
 * The hosted machine serves this renderer at its own origin, so a tab there
 * is a remote attachment with no native shell behind it: the API is
 * `window.location.origin`, and the bearer is whatever the page was handed.
 * Nothing here talks to the server; boot does that. This module is the one
 * place the two facts a page holds live — the handoff token it arrived with
 * and the machine it is attached to — so the callers that need them outside
 * React (boot, the machine state read, the gate) agree.
 */

/** Where a page came from, once boot has confirmed the origin is a machine. */
export type HostedSession = {
  /** The machine's origin, which is also this page's. */
  baseUrl: string;
  /**
   * The Model Gateway the machine authenticates against, from the machine's
   * own discovery document. `null` for a machine on static tokens, which has
   * no browser sign-in to send a reader to.
   */
  gatewayUrl: string | null;
  discovery?: import("./boot").AuthDiscovery;
};

/**
 * The one carrier a bearer may arrive in. A fragment never reaches the
 * server or its access log, and the page clears it before anything else can
 * read it. Tokens are URL-safe by construction; anything else is not a token.
 */
const HANDOFF_TOKEN = /^[A-Za-z0-9._~-]+$/;

/**
 * Why the machine's landing route could not hand the page a bearer. The
 * route words nothing itself; it lands the page with one of these and the
 * page does the talking.
 */
export type HandoffFailure = "expired" | "invalid" | "unavailable";
const HANDOFF_FAILURE_FRAGMENT =
  /^#handoff-failed=(expired|invalid|unavailable)$/;

let handoffToken: string | null = null;
let failure: HandoffFailure | null = null;
let session: HostedSession | null = null;

/**
 * Take the handoff bearer out of the page's fragment, if it arrived with one.
 *
 * Call this before the router exists: the router owns the fragment from then
 * on, and would read the token as a route. The token stays in memory for
 * this page's life — long enough for boot to retry — and nowhere else.
 */
export function captureHandoffToken(win: Window = window): void {
  const handoff = handoffEnvelope(win.location.hash);
  const failed = HANDOFF_FAILURE_FRAGMENT.exec(win.location.hash);
  if (!failed && !handoff) return;
  if (failed) failure = failed[1] as HandoffFailure;
  if (handoff) handoffToken = handoff.token;
  win.history.replaceState(
    win.history.state,
    "",
    `${win.location.pathname}${win.location.search}${handoff?.returnRoute ? `#${handoff.returnRoute}` : ""}`,
  );
}

function handoffEnvelope(
  hash: string,
): { token: string; returnRoute: string | null } | null {
  if (!hash.startsWith("#handoff=")) return null;
  const params = new URLSearchParams(hash.slice(1));
  const tokens = params.getAll("handoff");
  if (tokens.length !== 1 || !HANDOFF_TOKEN.test(tokens[0])) return null;
  const routes = params.getAll("return_to");
  const returnRoute =
    routes.length === 1 && isHandoffReturnRoute(routes[0]) ? routes[0] : null;
  return { token: tokens[0], returnRoute };
}

function isHandoffReturnRoute(route: string): boolean {
  return (
    route.startsWith("/") &&
    !route.startsWith("//") &&
    !route.startsWith("/\\") &&
    !route.includes("#") &&
    route.length <= 4096 &&
    !Array.from(route).some((character) =>
      /[\u0000-\u001f\u007f]/.test(character),
    )
  );
}

/** Hold a validated pasted bearer in the same tab-memory slot as a handoff. */
export function rememberHostedBearer(token: string): void {
  handoffToken = token;
  failure = null;
}

/** The bearer the page arrived with, or `null` if it opened without one. */
export function handoffBearer(): string | null {
  return handoffToken;
}

/** Why the page arrived without a bearer, when the landing route said. */
export function handoffFailure(): HandoffFailure | null {
  return failure;
}

/** Record that this page is served by the machine at `next.baseUrl`. */
export function markHostedSession(next: HostedSession | null): void {
  session = next;
}

/** The machine this page is served by, or `null` outside a hosted tab. */
export function hostedSession(): HostedSession | null {
  return session;
}

/** Test seam: forget both facts. */
export function resetHostedSessionForTests(): void {
  handoffToken = null;
  failure = null;
  session = null;
}

/**
 * Where the console signs a reader in and sends them back to this page:
 * the console's Tidebreak page with this page's hash-router route as
 * `return_to`, which the hand-off carries through to the landing page. A
 * connect card's approval page survives the round trip this way; the root
 * asks for no return route at all.
 */
export function consoleSignInUrl(
  gatewayUrl: string,
  win: Pick<Window, "location"> = window,
): string {
  const base = `${gatewayUrl.replace(/\/+$/, "")}/tidebreak`;
  const here = win.location.hash.startsWith("#/")
    ? win.location.hash.slice(1)
    : "/";
  return here === "/" ? base : `${base}?return_to=${encodeURIComponent(here)}`;
}

/** Where machine-owned OIDC starts and returns to the current route. */
export function oidcSignInUrl(
  startUrl: string,
  win: Pick<Window, "location"> = window,
): string {
  const url = new URL(startUrl, win.location.origin);
  const here = win.location.hash.startsWith("#/")
    ? win.location.hash.slice(1)
    : "/";
  if (here !== "/") url.searchParams.set("return_to", here);
  return url.toString();
}
