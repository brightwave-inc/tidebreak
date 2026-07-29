import { useCallback, useEffect, useState, type ReactNode } from "react";
import { ExternalLink, RefreshCw } from "lucide-react";

import type { ApiClient, GatewayStatus, ManagedPolicy } from "./api";
import { Button } from "@/components/ui/button";
import { Logomark } from "./Logomark";
import { openSignInPage } from "./openSignInPage";

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

  const reload = useCallback(async () => {
    const next = await client.getGatewayStatus();
    setStatus(next);
    setStatusError(null);
    return next;
  }, [client]);

  useEffect(() => {
    if (!managed) return;
    let cancelled = false;
    reload().catch((err) => {
      if (!cancelled) setStatusError(String(err));
    });
    return () => {
      cancelled = true;
    };
  }, [managed, reload]);

  // The watch is keyed on being managed, not on having a status: if the first
  // fetch failed, the ticks are what recover from it. Each tick retries, and
  // a success clears any stale error. One timer covers both directions —
  // pending → signed in lifts the gate, a sign-out anywhere lowers it again.
  const pendingFlow = status?.sign_in.state === "pending";
  useEffect(() => {
    if (!managed) return;
    const timer = window.setInterval(
      () => {
        void reload().catch(() => undefined);
      },
      pendingFlow ? PENDING_POLL_MS : SESSION_WATCH_MS,
    );
    return () => window.clearInterval(timer);
  }, [managed, pendingFlow, reload]);

  async function connect() {
    if (!policy) return;
    setWorking(true);
    setActionError(null);
    try {
      // Converge the provider config to the policy's locked gateway before
      // starting the flow: begin_sign_in refuses without a configured
      // provider, and the gate blocks the settings route that could fix it.
      // Convergence only ever writes the policy's own URL — toward the
      // policy, never away from it.
      const target = policy.gateway_url;
      if (
        target &&
        (!status?.configured ||
          !status.enabled ||
          !sameGateway(status.base_url, target))
      ) {
        await client.putProvider("model_gateway", {
          enabled: true,
          base_url: target,
        });
      }
      const started = await client.gatewaySignIn();
      await openSignInPage(started.authorization_url);
      await reload();
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

  if (!managed) return <>{children}</>;

  if (status === null && statusError === null) {
    return <BootScreen>starting…</BootScreen>;
  }

  // The gate lifts only for a session on the policy's own gateway: signed_in
  // reflects whatever provider URL is configured, which nothing pins to the
  // policy yet. A session on any other deployment stays gated.
  const lockedUrl = policy?.gateway_url ?? null;
  const sessionSatisfiesPolicy =
    status?.signed_in === true && sameGateway(status.base_url, lockedUrl);
  if (sessionSatisfiesPolicy) return <>{children}</>;

  // The device's managed gateway is the policy's URL, wherever the provider
  // config currently points.
  const shownUrl = lockedUrl ?? status?.base_url ?? null;
  const pendingUrl =
    status?.sign_in.state === "pending"
      ? status.sign_in.authorization_url
      : null;
  const failure =
    status?.sign_in.state === "failed" ? status.sign_in.message : null;

  return (
    <div className="boot" aria-label="Sign in required">
      <div className="boot-brand">
        <Logomark />
        <h1>OpenWave</h1>
      </div>
      <div className="welcome-copy">
        <h2>Sign in to continue</h2>
        <p>
          This OpenWave install is managed by your organization. Sign in to
          your model gateway to get started.
        </p>
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
          </a>
        </p>
      ) : (
        <Button type="button" disabled={working} onClick={() => void connect()}>
          <ExternalLink size={14} />
          Connect
        </Button>
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

function BootScreen({ children }: { children: ReactNode }) {
  return (
    <div className="boot">
      <div className="boot-brand">
        <Logomark />
        <h1>OpenWave</h1>
      </div>
      <p>{children}</p>
    </div>
  );
}
