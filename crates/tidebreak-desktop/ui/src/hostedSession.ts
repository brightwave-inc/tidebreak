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
};

/**
 * The one carrier a bearer may arrive in. A fragment never reaches the
 * server or its access log, and the page clears it before anything else can
 * read it. Tokens are URL-safe by construction; anything else is not a token.
 */
const HANDOFF_FRAGMENT = /^#handoff=([A-Za-z0-9._~-]+)$/;

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
  const failed = HANDOFF_FAILURE_FRAGMENT.exec(win.location.hash);
  const match = HANDOFF_FRAGMENT.exec(win.location.hash);
  if (!failed && !match) return;
  if (failed) failure = failed[1] as HandoffFailure;
  if (match) handoffToken = match[1];
  win.history.replaceState(
    win.history.state,
    "",
    `${win.location.pathname}${win.location.search}`,
  );
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
 * the console's Tidebreak page with this page's path as `return_to`, which
 * the hand-off carries through to the landing route. A connect card's
 * approval page survives the round trip this way; the root asks for no
 * return path at all.
 */
export function consoleSignInUrl(
  gatewayUrl: string,
  win: Pick<Window, "location"> = window,
): string {
  const base = `${gatewayUrl.replace(/\/+$/, "")}/tidebreak`;
  const here = `${win.location.pathname}${win.location.search}`;
  return here === "/" || here === ""
    ? base
    : `${base}?return_to=${encodeURIComponent(here)}`;
}
