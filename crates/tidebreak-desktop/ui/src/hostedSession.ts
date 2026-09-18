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
 *
 * The bearer stays in memory (decision 82). A presence-only sessionStorage
 * marker lets a reload renew through the existing console or OIDC hand-off
 * without writing the credential.
 */

/** What the machine's public discovery document says about signing in. */
export type AuthDiscovery =
  | { mode: "gateway"; gateway_url: string; resource: string }
  | { mode: "static_token" }
  | { mode: "oidc"; issuer_name: string; start_url: string }
  | { mode: "local" };

/** Where a page came from, once boot has confirmed the origin is a machine. */
export type HostedSession = {
  /** The machine's origin, which is also this page's. */
  baseUrl: string;
  /**
   * The Model Gateway the machine authenticates against, from the machine's
   * own discovery document. `null` for standalone token and OIDC machines,
   * whose browser sign-in starts on the machine itself.
   */
  gatewayUrl: string | null;
  /** How this machine signs a browser in, so the gate can offer it again. */
  discovery: AuthDiscovery;
};

/** Enough of `window` for hash-route and navigation seams in tests. */
export type HostedLocationWin = {
  location: { hash: string; href?: string; origin?: string };
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
/**
 * Presence-only marker that this tab held a machine session. Reload reads it
 * to renew through the console or OIDC start; the bearer never goes here.
 */
const HOSTED_CONTINUITY_KEY = "tidebreak.hostedSessionContinuity";
/**
 * Presence-only marker that this tab already sent its one automatic renewal.
 * Survives a slow or abandoned round trip so a missing bearer afterward is
 * sign-in, not another redirect, until a session is established or the
 * reader retries.
 */
const HOSTED_REENTRY_ATTEMPTED_KEY = "tidebreak.hostedReentryAttempted";
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

/**
 * Hold a bearer the reader pasted, in the same tab memory a hand-off bearer
 * lives in: no cookie, no storage, gone on reload. Boot has already probed it
 * against the machine, so this only records what was accepted.
 */
export function rememberHostedBearer(token: string): void {
  handoffToken = token;
  failure = null;
}

/** Whether a pasted value could be a bearer at all. Same alphabet a hand-off
 * bearer arrives in, so a stray paste is refused before any request. */
export function isHostedBearerShape(token: string): boolean {
  return token.length <= 512 && HANDOFF_TOKEN.test(token);
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
export function resetHostedSessionForTests(storage?: Storage | null): void {
  handoffToken = null;
  failure = null;
  session = null;
  handoffReturnedAt = null;
  draftsByRoute.clear();
  clearHostedContinuity(storage);
  clearHostedReentryAttempt(storage);
}

function sessionStorageOrNull(storage?: Storage | null): Storage | null {
  if (storage !== undefined) return storage;
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
}

function readStorageItem(key: string, storage?: Storage | null): string | null {
  try {
    return sessionStorageOrNull(storage)?.getItem(key) ?? null;
  } catch {
    return null;
  }
}

function writeStorageItem(
  key: string,
  value: string,
  storage?: Storage | null,
): void {
  try {
    sessionStorageOrNull(storage)?.setItem(key, value);
  } catch {
    // Continuity and loop-guard writes are best-effort; a blocked store
    // falls back to the in-memory facts for this document only.
  }
}

function removeStorageItem(key: string, storage?: Storage | null): void {
  try {
    sessionStorageOrNull(storage)?.removeItem(key);
  } catch {
    // Same as a missing key.
  }
}

function clearHostedContinuity(storage?: Storage | null): void {
  removeStorageItem(HOSTED_CONTINUITY_KEY, storage);
}

function clearHostedReentryAttempt(storage?: Storage | null): void {
  removeStorageItem(HOSTED_REENTRY_ATTEMPTED_KEY, storage);
}

function hostedSessionHasContinuity(storage?: Storage | null): boolean {
  return readStorageItem(HOSTED_CONTINUITY_KEY, storage) === "1";
}

function rememberHostedReentryAttempt(storage?: Storage | null): void {
  writeStorageItem(HOSTED_REENTRY_ATTEMPTED_KEY, "1", storage);
}

function hostedReentryAttempted(storage?: Storage | null): boolean {
  return readStorageItem(HOSTED_REENTRY_ATTEMPTED_KEY, storage) !== null;
}

/**
 * This tab held a bearer the machine accepted. Reload may renew through the
 * console or OIDC; the bearer itself stays in memory.
 */
export function noteHostedSessionEstablished(storage?: Storage | null): void {
  writeStorageItem(HOSTED_CONTINUITY_KEY, "1", storage);
  clearHostedReentryAttempt(storage);
}

/**
 * The reader asked to try signing in again. Clears the consumed automatic
 * renewal so the next reload may redirect once more. Continuity stays.
 */
export function allowHostedReentryRetry(storage?: Storage | null): void {
  clearHostedReentryAttempt(storage);
}

/**
 * Drop this tab's bearer and the reload-renewal marker. A later load shows
 * sign-in instead of silently signing back in — the sign-out contract.
 */
export function forgetHostedBrowserSession(storage?: Storage | null): void {
  handoffToken = null;
  failure = null;
  clearHostedContinuity(storage);
  clearHostedReentryAttempt(storage);
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

/**
 * True when a hand-off just landed this tab, or this tab already used its
 * one automatic renewal.
 */
export function hostedReentryIsLooping(
  now: number = Date.now(),
  storage?: Storage | null,
): boolean {
  if (handoffReturnedAt !== null && now - handoffReturnedAt < REENTRY_LOOP_MS) {
    return true;
  }
  return hostedReentryAttempted(storage);
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
  hosted: Pick<HostedSession, "baseUrl" | "gatewayUrl">,
  win: HostedLocationWin = window,
  now: number = Date.now(),
  storage?: Storage | null,
): "redirect" | "sign_in" {
  if (!hosted.gatewayUrl || hostedReentryIsLooping(now, storage)) {
    return "sign_in";
  }
  return beginHostedReentry(
    consoleSignInUrl(hosted.gatewayUrl, win),
    win,
    storage,
  );
}

/**
 * Reload of a tab that already held a session: renew through the same
 * console or OIDC path the sign-in buttons use, keeping the hash route.
 *
 * A first visit, a hand-off that already failed, a sign-out, a static-token
 * machine, or a loop after a missing gateway login stays on sign-in.
 */
export function reenterReloadedHostedSession(
  hosted: HostedSession | null,
  failure: HandoffFailure | null = null,
  win: HostedLocationWin = window,
  now: number = Date.now(),
  storage?: Storage | null,
): "redirect" | "sign_in" {
  if (!hosted || failure || !hostedSessionHasContinuity(storage)) {
    return "sign_in";
  }
  if (hostedReentryIsLooping(now, storage)) return "sign_in";
  const url = hostedRenewalUrl(hosted, win);
  if (!url) return "sign_in";
  return beginHostedReentry(url, win, storage);
}

function hostedRenewalUrl(
  hosted: HostedSession,
  win: HostedLocationWin,
): string | null {
  if (hosted.discovery.mode === "gateway" && hosted.gatewayUrl) {
    return consoleSignInUrl(hosted.gatewayUrl, win);
  }
  if (hosted.discovery.mode === "oidc") {
    const origin = win.location.origin ?? window.location.origin;
    return oidcSignInUrl(hosted.discovery.start_url, {
      location: { origin, hash: win.location.hash },
    });
  }
  return null;
}

function beginHostedReentry(
  url: string,
  win: HostedLocationWin,
  storage?: Storage | null,
): "redirect" {
  rememberHostedReentryAttempt(storage);
  win.location.href = url;
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

/**
 * Where a machine-owned OIDC sign-in starts, carrying this tab's route back.
 *
 * The machine publishes the start URL in its discovery document, and this
 * adds `return_to` the way {@link consoleSignInUrl} does, so a session link
 * opened in a fresh browser lands on the session rather than the root.
 */
export function oidcSignInUrl(
  startUrl: string,
  win: HostedLocationWin & { location: { origin: string } } = window,
): string {
  const url = new URL(startUrl, win.location.origin);
  const here = hostedHashRoute(win);
  if (here !== "/") url.searchParams.set("return_to", here);
  return url.toString();
}
