import { useCallback, useEffect, useState, type ReactNode } from "react";
import { ExternalLink, Laptop, RefreshCw } from "lucide-react";

import type { ApiClient, GatewayStatus, ManagedPolicy } from "./api";
import { Button } from "@/components/ui/button";
import { HostedSignIn } from "./HostedSignIn";
import { Logomark } from "./Logomark";
import { ManagedPolicyContext } from "./managedPolicy";
import { hasNativeHost, onPairingChanged } from "./host";
import { hostedSession } from "./hostedSession";
import { openInBrowser } from "./openInBrowser";
import { disconnectRemoteMachine, remoteMachineState } from "./remoteMachine";
import { useVisibilityGatedPoll } from "./useVisibilityGatedPoll";
import { WindowDragStrip } from "./WindowDragStrip";

/** While the browser flow is pending the exchange lands out of band, so the
 * poll is what turns the gate off. Matches the settings panel's cadence. */
const PENDING_POLL_MS = 2_000;
/** The session watch otherwise: signed in, it is what returns a signed-out
 * reader to the gate; signed out, it notices sign-ins completed elsewhere.
 * The shell's pairing nudge is the prompt path, so this is a safety net, and
 * it pauses while the window is hidden. */
const SESSION_WATCH_MS = 60_000;
/** A policy read that hangs is a transport failure, not an answer. */
const POLICY_TIMEOUT_MS = 5_000;
const POLICY_RETRY_MIN_MS = 1_000;
const POLICY_RETRY_MAX_MS = 15_000;

type PolicyState =
  | { kind: "loading" }
  | { kind: "resolved"; policy: ManagedPolicy }
  /** The server answered, and the answer was an error: the policy exists but
   * cannot be read. A profile that claims to be managed must never quietly
   * revert to the open experience, so this blocks instead of failing open.
   * The status is kept because one answer means something specific to a
   * hosted browser tab: a refused bearer is a session that ended. */
  | { kind: "blocked"; status: number };

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
    (a.hosted_gateway_url ?? null) === (b.hosted_gateway_url ?? null) &&
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
  // The machine this window is pointed at, or `null` for this computer.
  //
  // Read from the shell rather than through the API client on purpose. When
  // the client is attached to a machine that refuses it — a gateway session
  // that ended, access revoked — every read through the client fails, and
  // this is the one fact still legible. It is also what makes the escape
  // hatch below possible: detaching is a host command, so it works no matter
  // what the machine says.
  const [attachedMachine, setAttachedMachine] = useState<string | null>(null);

  const policy = policyState.kind === "resolved" ? policyState.policy : null;
  const managed = policy?.managed === true;
  // A deep-link pairing awaiting the sign-in that is its consent. The gate
  // presents it exactly like the managed sign-in — full-window, nothing of
  // the app behind it — because from the user's seat it is the same step:
  // sign in to the named gateway. The one durable difference (the sign-in
  // commits the provision) is the server's business. On a managed profile a
  // pending pairing is a *re*-pair the user confirmed in the shell's native
  // dialog, and it must surface even over a signed-in session — dismissing
  // it ("Not now") is what returns the app.
  const pendingPairingUrl = policy?.pending_gateway_url ?? null;
  const gateActive = managed || pendingPairingUrl !== null;

  // Once, on mount. The attachment only changes by a path that reloads the
  // window, so there is nothing here to watch.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const state = await remoteMachineState();
        if (!cancelled) setAttachedMachine(state.baseUrl);
      } catch {
        // Not knowing leaves the local copy below, which is the safe read:
        // it offers no escape hatch rather than a broken one.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

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
            policy: {
              managed: false,
              source: "unmanaged",
              misconfigured: false,
              allow_local_mcp_servers: false,
            },
          });
          return;
        }
        if (httpStatus !== null) {
          setPolicyState({ kind: "blocked", status: httpStatus });
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
  // that had already started refusing them. `/policy` is a local read; the
  // shell's pairing nudge below is the prompt path and this cadence is the
  // net under it.
  const watching = policyState.kind === "resolved";
  const refreshPolicy = useCallback(() => {
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
        const httpStatus = httpStatusOf(err);
        if (httpStatus !== null && httpStatus !== 404) {
          setPolicyState({ kind: "blocked", status: httpStatus });
        }
      });
  }, [client]);
  useVisibilityGatedPoll(refreshPolicy, SESSION_WATCH_MS, {
    enabled: watching,
  });

  // The shell nudges when a provision link parks a pending pairing or a
  // confirmed re-pair replaces one; refetching now instead of on the next
  // tick is what makes the gate appear promptly after the user clicks a
  // link or confirms the native dialog. The poll above stays the fallback —
  // it still covers MDM flips and a nudge that fired before this
  // subscription existed (cold start by link).
  useEffect(() => {
    if (!watching) return;
    return onPairingChanged(refreshPolicy);
  }, [watching, refreshPolicy]);

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
  // Hidden, it waits: the reader returning is what shows the gate, and the
  // read on return is what it sees.
  const pendingFlow = status?.sign_in.state === "pending";
  const refreshStatus = useCallback(() => {
    void reload().catch(() => undefined);
  }, [reload]);
  useVisibilityGatedPoll(
    refreshStatus,
    pendingFlow ? PENDING_POLL_MS : SESSION_WATCH_MS,
    { enabled: gateActive },
  );

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
      await openInBrowser(started.authorization_url);
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
    // A hosted browser tab whose bearer the machine stopped accepting. There
    // is no shell to mint another and no local server to fall back to; the
    // way back in is the console that signed the reader in the first time.
    const hosted = hostedSession();
    if (
      hosted !== null &&
      !hasNativeHost() &&
      policyState.kind === "blocked" &&
      (policyState.status === 401 || policyState.status === 403)
    ) {
      return (
        <HostedSignIn
          reason="session_ended"
          machineUrl={hosted.baseUrl}
          discovery={
            hosted.discovery ??
            (hosted.gatewayUrl
              ? {
                  mode: "gateway",
                  gateway_url: hosted.gatewayUrl,
                  resource: "tidebreak",
                }
              : { mode: "static_token" })
          }
        />
      );
    }
    // Attached to a machine, and it will not answer. Naming the managed
    // policy here would be a lie — the policy on this computer is fine, and
    // the reader cannot act on the machine's copy anyway. What they can
    // always do is come back to this computer, and until this screen offered
    // it there was no way to: the Machine settings sit behind this gate, so
    // a machine that stopped accepting the session locked the window with
    // nothing but a Retry that retried the same refusal.
    if (attachedMachine !== null) {
      return (
        <div className="boot" aria-label="Machine unavailable">
          <WindowDragStrip />
          <div className="boot-brand">
            <Logomark />
            <h1>Tidebreak</h1>
          </div>
          <div className="welcome-copy">
            <h2>That machine is not answering</h2>
            <p>
              This window works on{" "}
              <code className="font-medium">{attachedMachine}</code>, and that
              machine refused it. Your access may have ended, or the machine may
              be down.
            </p>
            <p>
              Come back to this computer to sign in again. Work on the machine
              keeps running, and you can reattach once it answers.
            </p>
          </div>
          <div className="flex items-center gap-2">
            {hasNativeHost() && (
              <Button
                type="button"
                disabled={working}
                onClick={() =>
                  void (async () => {
                    setWorking(true);
                    setActionError(null);
                    try {
                      await disconnectRemoteMachine();
                      // Same reason the settings panel reloads: the API
                      // client and the event stream were built against the
                      // machine this window opened on.
                      window.location.reload();
                    } catch (err) {
                      setActionError(String(err));
                      setWorking(false);
                    }
                  })()
                }
              >
                <Laptop size={14} />
                Work on this computer
              </Button>
            )}
            <Button
              type="button"
              variant="ghost"
              disabled={working}
              onClick={() => setPolicyState({ kind: "loading" })}
            >
              <RefreshCw size={14} />
              Retry
            </Button>
          </div>
          {actionError && <p className="boot-error-detail">{actionError}</p>}
        </div>
      );
    }
    return (
      <div className="boot" aria-label="Managed policy unavailable">
        <WindowDragStrip />
        <div className="boot-brand">
          <Logomark />
          <h1>Tidebreak</h1>
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
  // managed server-side, and the managed branch takes over on the next poll
  // — and it holds the gate down even over a satisfied session, or a
  // confirmed re-pair would be invisible exactly when the old gateway is
  // still signed in.
  const pairing = pendingPairingUrl !== null;
  const lockedUrl = policy?.gateway_url ?? null;
  const sessionSatisfiesPolicy =
    managed &&
    !pairing &&
    status?.signed_in === true &&
    sameGateway(status.base_url, lockedUrl);
  if (sessionSatisfiesPolicy)
    return <Published policy={policy}>{children}</Published>;
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
        <h1>Tidebreak</h1>
      </div>
      <div className="welcome-copy">
        {pairing ? (
          <>
            <h2>Connect to your model gateway</h2>
            <p>
              Sign in to connect Tidebreak to the gateway below, which will
              manage this device.
            </p>
            {managed && lockedUrl && (
              <p>
                This replaces <code className="font-medium">{lockedUrl}</code>,
                which currently manages this device.
              </p>
            )}
          </>
        ) : (
          <>
            <h2>Sign in to continue</h2>
            <p>
              This device is managed by your organization. Sign in to get
              started.
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
            rel="noreferrer noopener"
            onClick={(event) => {
              // No target="_blank": the shell plugin's injected click handler
              // opens such links itself without honoring preventDefault,
              // which doubled this one. Route through the native opener and
              // keep the href for hover/copy.
              event.preventDefault();
              void openInBrowser(pendingUrl);
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
        Sign-in opens in your browser. Tidebreak never sees your password.
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
        <h1>Tidebreak</h1>
      </div>
      <p>{children}</p>
    </div>
  );
}
