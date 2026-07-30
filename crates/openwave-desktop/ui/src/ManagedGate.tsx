import { useCallback, useEffect, useState, type ReactNode } from "react";
import { ExternalLink, RefreshCw } from "lucide-react";

import type { ApiClient, GatewayStatus, ManagedPolicy } from "./api";
import { Button } from "@/components/ui/button";
import { Logomark } from "./Logomark";
import { ManagedPolicyContext } from "./managedPolicy";
import { openSignInPage } from "./openSignInPage";
import { WindowDragStrip } from "./WindowDragStrip";

/** While the browser flow is pending the exchange lands out of band, so the
 * poll is what turns the gate off. Matches the settings panel's cadence. */
const PENDING_POLL_MS = 2_000;
/** The session watch otherwise: signed in, it is what returns a signed-out
 * reader to the gate; signed out, it notices sign-ins completed elsewhere. */
const SESSION_WATCH_MS = 5_000;
/** A policy read that hangs is a transport failure, not an answer. */
const POLICY_TIMEOUT_MS = 5_000;
const POLICY_RETRY_MIN_MS = 1_000;
const POLICY_RETRY_MAX_MS = 15_000;

type PolicyState =
  | { kind: "loading" }
  | { kind: "resolved"; policy: ManagedPolicy }
  /** The server answered, and the answer was an error: the policy exists but
   * cannot be read. A profile that claims to be managed must never quietly
   * revert to the open experience, so this blocks instead of failing open. */
  | { kind: "blocked" };

/** ApiClient throws `Error("<status>: <detail>")` for an HTTP error response;
 * everything else — a rejected fetch, the timeout — is transport-level. */
function httpStatusOf(err: unknown): number | null {
  if (!(err instanceof Error)) return null;
  const match = /^(\d{3}): /.exec(err.message);
  return match ? Number(match[1]) : null;
}

function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = window.setTimeout(
      () => reject(new Error(`timed out after ${ms}ms`)),
      ms,
    );
    promise.then(
      (value) => {
        window.clearTimeout(timer);
        resolve(value);
      },
      (err) => {
        window.clearTimeout(timer);
        reject(err);
      },
    );
  });
}

/** Gateway URLs compare by parsed identity, not by string: the policy stores
 * its URL normalized with a trailing slash, provider config may not. */
function normalizedGatewayUrl(raw: string | null | undefined): string | null {
  if (!raw) return null;
  try {
    const url = new URL(raw);
    return `${url.protocol}//${url.host}${url.pathname.replace(/\/+$/, "")}`;
  } catch {
    return null;
  }
}

function sameGateway(
  a: string | null | undefined,
  b: string | null | undefined,
): boolean {
  const left = normalizedGatewayUrl(a);
  return left !== null && left === normalizedGatewayUrl(b);
}

/** Whether two resolutions say the same thing. The watch re-renders the whole
 * app below the gate, so an unchanged answer must be a no-op. */
function samePolicy(a: ManagedPolicy, b: ManagedPolicy): boolean {
  return (
    a.managed === b.managed &&
    a.source === b.source &&
    a.misconfigured === b.misconfigured &&
    (a.gateway_url ?? null) === (b.gateway_url ?? null) &&
    (a.pending_gateway_url ?? null) === (b.pending_gateway_url ?? null)
  );
}

/**
 * The managed-mode sign-in gate.
 *
 * When the resolved policy reports this install as managed, nothing of the
 * app renders until a gateway session exists on the policy's own gateway.
 * The gate wraps the router rather than living on a route, so no navigation
 * — settings included — can get around it. Sign-out, wherever it happens,
 * brings the gate back within one watch interval. Unmanaged profiles render
 * children untouched, and the gateway is never even asked for its status.
 *
 * The policy read fails closed. A transport failure (server not up yet,
 * request timed out) retries silently behind the boot screen — an
 * unreachable server is not an unmanaged profile. An error response blocks
 * the app outright: the server actively said the policy is unreadable.
 */
export function ManagedGate({
  client,
  children,
}: {
  client: ApiClient;
  children: ReactNode;
}) {
  const [policyState, setPolicyState] = useState<PolicyState>({
    kind: "loading",
  });
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [working, setWorking] = useState(false);
  // Two error channels on purpose: the watch clears its own stale fetch
  // errors on a successful poll, which must not wipe a Connect failure the
  // reader is still looking at.
  const [statusError, setStatusError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const policy = policyState.kind === "resolved" ? policyState.policy : null;
  const managed = policy?.managed === true;
  // A deep-link pairing awaiting the sign-in that is its consent. The gate
  // presents it exactly like the managed sign-in — full-window, nothing of
  // the app behind it — because from the user's seat it is the same step:
  // sign in to the named gateway. The one durable difference (the sign-in
  // commits the provision) is the server's business.
  const pendingPairingUrl = !managed ? (policy?.pending_gateway_url ?? null) : null;
  const gateActive = managed || pendingPairingUrl !== null;

  useEffect(() => {
    if (policyState.kind !== "loading") return;
    let cancelled = false;
    let timer: number | undefined;
    const attempt = async (backoffMs: number) => {
      try {
        const next = await withTimeout(client.getPolicy(), POLICY_TIMEOUT_MS);
        if (!cancelled) setPolicyState({ kind: "resolved", policy: next });
      } catch (err) {
        if (cancelled) return;
        const httpStatus = httpStatusOf(err);
        if (httpStatus === 404) {
          // No policy layer at all — a renderer newer than its server (the
          // browser dev path). There is no policy to enforce, so the open
          // product is correct, not a dead end.
          setPolicyState({
            kind: "resolved",
            policy: { managed: false, source: "unmanaged", misconfigured: false },
          });
          return;
        }
        if (httpStatus !== null) {
          setPolicyState({ kind: "blocked" });
          return;
        }
        timer = window.setTimeout(() => {
          void attempt(Math.min(backoffMs * 2, POLICY_RETRY_MAX_MS));
        }, backoffMs);
      }
    };
    void attempt(POLICY_RETRY_MIN_MS);
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [client, policyState.kind]);

  // Policy is watched, not read once. A profile can become managed while the
  // app is running — an MDM push, or the deep-link pairing flow mid-session —
  // and until this the renderer went on presenting the whole open surface:
  // no sign-in gate, and Providers and MCP still editable against a server
  // that had already started refusing them. `/policy` is a local read, so the
  // session cadence is affordable.
  const watching = policyState.kind === "resolved";
  useEffect(() => {
    if (!watching) return;
    const timer = window.setInterval(() => {
      void client
        .getPolicy()
        .then((next) => {
          setPolicyState((current) =>
            current.kind === "resolved" && samePolicy(current.policy, next)
              ? current
              : { kind: "resolved", policy: next },
          );
        })
        .catch((err) => {
          // An error *response* means the server says the policy is
          // unreadable, which fails closed exactly as it does on first read.
          // A transport failure says nothing, so the last answer stands.
          if (httpStatusOf(err) !== null && httpStatusOf(err) !== 404) {
            setPolicyState({ kind: "blocked" });
          }
        });
    }, SESSION_WATCH_MS);
    return () => window.clearInterval(timer);
  }, [client, watching]);

  const reload = useCallback(async () => {
    const next = await client.getGatewayStatus();
    setStatus(next);
    setStatusError(null);
    return next;
  }, [client]);

  useEffect(() => {
    if (!gateActive) return;
    let cancelled = false;
    reload().catch((err) => {
      if (!cancelled) setStatusError(String(err));
    });
    return () => {
      cancelled = true;
    };
  }, [gateActive, reload]);

  // The watch is keyed on being managed, not on having a status: if the first
  // fetch failed, the ticks are what recover from it. Each tick retries, and
  // a success clears any stale error. One timer covers both directions —
  // pending → signed in lifts the gate, a sign-out anywhere lowers it again.
  const pendingFlow = status?.sign_in.state === "pending";
  useEffect(() => {
    if (!gateActive) return;
    const timer = window.setInterval(
      () => {
        void reload().catch(() => undefined);
      },
      pendingFlow ? PENDING_POLL_MS : SESSION_WATCH_MS,
    );
    return () => window.clearInterval(timer);
  }, [gateActive, pendingFlow, reload]);

  async function connect() {
    if (!policy) return;
    setWorking(true);
    setActionError(null);
    try {
      // Sign-in needs no provider convergence: the server derives the
      // deployment from the policy itself — or from the pending pairing the
      // shell registered — and the retired provider row is not writable at
      // all.
      const started = await client.gatewaySignIn();
      await openSignInPage(started.authorization_url);
      await reload();
    } catch (err) {
      setActionError(String(err));
    } finally {
      setWorking(false);
    }
  }

  async function dismissPairing() {
    setWorking(true);
    setActionError(null);
    try {
      const next = await client.dismissGatewayPairing();
      setPolicyState({ kind: "resolved", policy: next });
    } catch (err) {
      setActionError(String(err));
    } finally {
      setWorking(false);
    }
  }

  if (policyState.kind === "loading") return <BootScreen>starting…</BootScreen>;

  // A managed policy without a gateway URL has nothing the gate could ever
  // be satisfied against, so it blocks rather than lifting on any session.
  // `misconfigured` is read defensively: the MDM-readers slice adds it to
  // the wire type, and a renderer running ahead of that must already honor
  // it rather than fail open.
  const policyMisconfigured =
    policy !== null &&
    ((policy as { misconfigured?: boolean }).misconfigured === true ||
      (policy.managed && !policy.gateway_url));

  if (policyState.kind === "blocked" || policyMisconfigured) {
    return (
      <div className="boot" aria-label="Managed policy unavailable">
        <WindowDragStrip />
        <div className="boot-brand">
          <Logomark />
          <h1>OpenWave</h1>
        </div>
        <div className="welcome-copy">
          <h2>Managed policy unavailable</h2>
          <p>
            This device&apos;s managed policy is misconfigured. Contact your
            administrator.
          </p>
        </div>
        <Button
          type="button"
          onClick={() => setPolicyState({ kind: "loading" })}
        >
          <RefreshCw size={14} />
          Retry
        </Button>
      </div>
    );
  }

  // Everything below the gate reads the same resolved policy the gate did:
  // the settings surfaces gate themselves on it, and re-fetching `/policy`
  // per panel could only produce disagreement.
  if (!gateActive) return <Published policy={policy}>{children}</Published>;

  if (status === null && statusError === null) {
    return <BootScreen>starting…</BootScreen>;
  }

  // The gate lifts only for a session on the policy's own gateway. The
  // server already pins signed_in to the policy URL; the comparison stays as
  // defense in depth for a renderer running against an older server. A
  // pending pairing never lifts here — its sign-in flips the policy to
  // managed server-side, and the managed branch takes over on the next poll.
  const lockedUrl = policy?.gateway_url ?? null;
  const sessionSatisfiesPolicy =
    managed &&
    status?.signed_in === true &&
    sameGateway(status.base_url, lockedUrl);
  if (sessionSatisfiesPolicy) return <Published policy={policy}>{children}</Published>;

  const pairing = pendingPairingUrl !== null;
  // The device's managed gateway is the policy's URL — or, for a pairing
  // awaiting consent, the URL the shell registered from the provision link.
  const shownUrl = pairing
    ? pendingPairingUrl
    : (lockedUrl ?? status?.base_url ?? null);
  const pendingUrl =
    status?.sign_in.state === "pending"
      ? status.sign_in.authorization_url
      : null;
  const failure =
    status?.sign_in.state === "failed" ? status.sign_in.message : null;

  return (
    <div
      className="boot"
      aria-label={pairing ? "Gateway pairing requested" : "Sign in required"}
    >
      <WindowDragStrip />
      <div className="boot-brand">
        <Logomark />
        <h1>OpenWave</h1>
      </div>
      <div className="welcome-copy">
        {pairing ? (
          <>
            <h2>Connect to your model gateway</h2>
            <p>
              Signing in connects OpenWave to the gateway below. It will
              manage this device — it controls which models are available —
              and the connection cannot be undone from within OpenWave.
            </p>
          </>
        ) : (
          <>
            <h2>Sign in to continue</h2>
            <p>
              This OpenWave install is managed by your organization. Sign in
              to your model gateway to get started.
            </p>
          </>
        )}
      </div>
      {shownUrl && (
        <p className="text-muted-foreground text-sm">
          Gateway <code className="font-medium">{shownUrl}</code>
        </p>
      )}
      {failure && (
        <p className="text-destructive text-sm" role="alert">
          {failure}
        </p>
      )}
      {pendingUrl ? (
        <p className="text-sm">
          Waiting for the browser…{" "}
          <a
            className="underline"
            href={pendingUrl}
            target="_blank"
            rel="noreferrer noopener"
            onClick={(event) => {
              // The webview swallows target="_blank"; route through the
              // native opener and keep the href for hover/copy.
              event.preventDefault();
              void openSignInPage(pendingUrl);
            }}
          >
            Open the sign-in page again
          </a>{" "}
          ·{" "}
          {/* A stalled flow otherwise blocks the gate for the full sign-in
              timeout. Starting over begins a fresh attempt; the server
              invalidates the abandoned one, so a late completion of it
              cannot sign the device in. */}
          <button
            type="button"
            className="underline disabled:opacity-50"
            disabled={working}
            onClick={() => void connect()}
          >
            Start over
          </button>
        </p>
      ) : (
        <div className="flex items-center gap-2">
          <Button
            type="button"
            disabled={working}
            onClick={() => void connect()}
          >
            <ExternalLink size={14} />
            {pairing ? "Sign in and connect" : "Connect"}
          </Button>
          {pairing && (
            <Button
              type="button"
              variant="ghost"
              disabled={working}
              onClick={() => void dismissPairing()}
            >
              Not now
            </Button>
          )}
        </div>
      )}
      {actionError && <p className="boot-error-detail">{actionError}</p>}
      {statusError && <p className="boot-error-detail">{statusError}</p>}
      <p className="text-muted-foreground max-w-md text-xs leading-relaxed">
        Sign-in happens in your browser against the gateway itself; OpenWave
        never sees your identity provider credentials.
      </p>
    </div>
  );
}

function Published({
  policy,
  children,
}: {
  policy: ManagedPolicy | null;
  children: ReactNode;
}) {
  if (policy === null) return <>{children}</>;
  return (
    <ManagedPolicyContext.Provider value={policy}>
      {children}
    </ManagedPolicyContext.Provider>
  );
}

function BootScreen({ children }: { children: ReactNode }) {
  return (
    <div className="boot">
      <WindowDragStrip />
      <div className="boot-brand">
        <Logomark />
        <h1>OpenWave</h1>
      </div>
      <p>{children}</p>
    </div>
  );
}
