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

/** Enough of `window` for hash-route and navigation seams in tests. */
export type HostedLocationWin = {
  location: { hash: string; href?: string };
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
/** When this tab last landed through a hand-off. In memory only: a loop
 * guard, not a session. */
let handoffReturnedAt: number | null = null;
/** Unsent composer text keyed by hash route, for a re-entry that stays in
 * this document. A full-page navigation also writes it to sessionStorage. */
const draftsByRoute = new Map<string, string>();

const HOSTED_REENTRY_DRAFT_PREFIX = "tidebreak.hostedReentryDraft:";
/** A second refusal this soon after a hand-off is a loop, not a new hour. */
const REENTRY_LOOP_MS = 15_000;

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
  if (handoff) {
    handoffToken = handoff.token;
    handoffReturnedAt = Date.now();
  }
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
  handoffReturnedAt = null;
  draftsByRoute.clear();
}

function sessionStorageOrNull(storage?: Storage | null): Storage | null {
  if (storage !== undefined) return storage;
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
}

function draftStorageKey(route: string): string {
  return `${HOSTED_REENTRY_DRAFT_PREFIX}${route}`;
}

/**
 * Keep an unsent composer draft across hosted re-entry. Memory covers an
 * in-page navigation; sessionStorage covers a full-page trip to the console
 * and back. The bearer never goes here.
 */
export function stashComposerDraftForReentry(
  route: string,
  draft: string,
  storage?: Storage | null,
): void {
  if (!draft) return;
  draftsByRoute.set(route, draft);
  try {
    sessionStorageOrNull(storage)?.setItem(draftStorageKey(route), draft);
  } catch {
    // A lost draft is not a lost session.
  }
}

/**
 * Read the draft stashed for `route` once, then forget it. Memory wins when
 * both are present.
 */
export function takeComposerDraftForReentry(
  route: string,
  storage?: Storage | null,
): string | null {
  const fromMemory = draftsByRoute.get(route) ?? null;
  draftsByRoute.delete(route);
  const store = sessionStorageOrNull(storage);
  let fromStore: string | null = null;
  try {
    const key = draftStorageKey(route);
    fromStore = store?.getItem(key) ?? null;
    store?.removeItem(key);
  } catch {
    // Missing storage is the same as no draft.
  }
  return fromMemory || fromStore;
}

/** True when a hand-off just landed this tab and another refusal is a loop. */
export function hostedReentryIsLooping(now: number = Date.now()): boolean {
  return (
    handoffReturnedAt !== null && now - handoffReturnedAt < REENTRY_LOOP_MS
  );
}

/** This tab's hash-router path, or `/` when the fragment is not a route. */
export function hostedHashRoute(win: HostedLocationWin = window): string {
  return win.location.hash.startsWith("#/") ? win.location.hash.slice(1) : "/";
}

/**
 * A gateway machine whose bearer died: send the tab to the console unless
 * we just came back that way. Returns `"redirect"` after assigning
 * `win.location.href`, or `"sign_in"` when the dead-end screen should
 * render (standalone machine, or a loop).
 */
export function reenterExpiredHostedSession(
  hosted: HostedSession,
  win: HostedLocationWin = window,
  now: number = Date.now(),
): "redirect" | "sign_in" {
  if (!hosted.gatewayUrl || hostedReentryIsLooping(now)) return "sign_in";
  win.location.href = consoleSignInUrl(hosted.gatewayUrl, win);
  return "redirect";
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
  win: HostedLocationWin = window,
): string {
  const base = `${gatewayUrl.replace(/\/+$/, "")}/tidebreak`;
  const here = win.location.hash.startsWith("#/")
    ? win.location.hash.slice(1)
    : "/";
  return here === "/" ? base : `${base}?return_to=${encodeURIComponent(here)}`;
}
