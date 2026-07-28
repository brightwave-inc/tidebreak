import { useCallback, useEffect, useState, type ReactNode } from "react";
import { ExternalLink } from "lucide-react";

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

/**
 * The managed-mode sign-in gate.
 *
 * When the resolved policy reports this install as managed, nothing of the
 * app renders until a gateway session exists. The gate wraps the router
 * rather than living on a route, so no navigation — settings included — can
 * get around it. Sign-out, wherever it happens, brings the gate back within
 * one watch interval. Unmanaged profiles render children untouched, and the
 * gateway is never even asked for its status.
 *
 * The gate is presentation only: enforcement is the server's managed
 * lockdown, which is why an unreadable policy fails open to the ordinary app
 * instead of bricking it.
 */
export function ManagedGate({
  client,
  children,
}: {
  client: ApiClient;
  children: ReactNode;
}) {
  // undefined: still resolving; null: unreadable, treated as unmanaged.
  const [policy, setPolicy] = useState<ManagedPolicy | null | undefined>();
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const managed = policy?.managed === true;

  useEffect(() => {
    let cancelled = false;
    client.getPolicy().then(
      (next) => {
        if (!cancelled) setPolicy(next);
      },
      () => {
        if (!cancelled) setPolicy(null);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [client]);

  const reload = useCallback(async () => {
    const next = await client.getGatewayStatus();
    setStatus(next);
    return next;
  }, [client]);

  useEffect(() => {
    if (!managed) return;
    let cancelled = false;
    reload().catch((err) => {
      if (!cancelled) setError(String(err));
    });
    return () => {
      cancelled = true;
    };
  }, [managed, reload]);

  // One timer covers both directions: pending → signed in lifts the gate, and
  // a sign-out anywhere lowers it again.
  const signInState = status?.sign_in.state;
  useEffect(() => {
    if (!managed || signInState === undefined) return;
    const timer = window.setInterval(
      () => {
        void reload().catch(() => undefined);
      },
      signInState === "pending" ? PENDING_POLL_MS : SESSION_WATCH_MS,
    );
    return () => window.clearInterval(timer);
  }, [managed, signInState, reload]);

  async function connect() {
    setWorking(true);
    setError(null);
    try {
      const started = await client.gatewaySignIn();
      await openSignInPage(started.authorization_url);
      await reload();
    } catch (err) {
      setError(String(err));
    } finally {
      setWorking(false);
    }
  }

  if (!managed) {
    // Hold the boot screen while the policy resolves, so a managed install
    // never flashes the open product before the gate can assert itself.
    if (policy === undefined) return <BootScreen>starting…</BootScreen>;
    return <>{children}</>;
  }
  if (status === null && error === null) {
    return <BootScreen>starting…</BootScreen>;
  }
  if (status?.signed_in) return <>{children}</>;

  const lockedUrl = policy?.gateway_url ?? status?.base_url ?? null;
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
      {lockedUrl && (
        <p className="text-muted-foreground text-sm">
          Gateway <code className="font-medium">{lockedUrl}</code>
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
      {error && <p className="boot-error-detail">{error}</p>}
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
