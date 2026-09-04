import { ExternalLink, RotateCw } from "lucide-react";
import { useState, type FormEvent } from "react";

import { Button } from "@/components/ui/button";
import {
  consoleSignInUrl,
  oidcSignInUrl,
  type HandoffFailure,
} from "./hostedSession";
import type { AuthDiscovery } from "./boot";
import { Input } from "@/components/ui/input";
import { Logomark } from "./Logomark";
import { WindowDragStrip } from "./WindowDragStrip";

/**
 * Why a hosted browser tab has no session to show the app with.
 *
 * `no_session`: the page opened without a bearer — someone typed the
 * machine's address, or reloaded a tab. `session_ended`: the bearer the page
 * held stopped being accepted, which after an hour is simply its lifetime.
 * `handoff_failed`: the page came from the machine's landing route and the
 * route could not turn the console's code into a bearer; `failure` says why.
 */
export type HostedSignInReason =
  | "no_session"
  | "session_ended"
  | "handoff_failed";

export type HostedSignInProps = {
  reason: HostedSignInReason;
  failure?: HandoffFailure | null;
  /** The machine this page is served by. */
  machineUrl: string;
  /**
   * The console that can sign the reader in here. `null` when the machine
   * has no gateway, in which case the page can only say where to go instead.
   */
  discovery: AuthDiscovery;
  onToken?: (token: string) => Promise<void>;
  onRetry?: () => void;
};

/**
 * The screen a hosted browser tab shows until it holds a session.
 *
 * Gateway machines send the reader through the console. Standalone machines
 * either validate an administrator-provided token or start OIDC on the machine.
 * Every successful path leaves the bearer only in this tab's memory.
 */
function handoffFailureCopy(failure: HandoffFailure | null | undefined): {
  title: string;
  detail: string;
} {
  switch (failure) {
    case "expired":
      return {
        title: "That sign-in link has expired",
        detail:
          "A link from the console works once, within a minute of being made.",
      };
    case "unavailable":
      return {
        title: "This machine could not reach your sign-in provider",
        detail:
          "Sign-in needs the provider to answer, and it did not. Nothing about your account has changed.",
      };
    default:
      return {
        title: "That sign-in link is not valid",
        detail:
          "The sign-in response was refused or changed on the way back to this machine.",
      };
  }
}

export function HostedSignIn({
  reason,
  failure = null,
  machineUrl,
  discovery,
  onToken,
  onRetry = () => window.location.reload(),
}: HostedSignInProps) {
  const failed =
    reason === "handoff_failed" ? handoffFailureCopy(failure) : null;
  const [token, setToken] = useState("");
  const [tokenError, setTokenError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const gatewayUrl =
    discovery.mode === "gateway" ? discovery.gateway_url : null;
  async function submitToken(event: FormEvent) {
    event.preventDefault();
    if (!onToken || !token.trim()) return;
    setSubmitting(true);
    setTokenError(null);
    try {
      await onToken(token.trim());
    } catch {
      setTokenError("This machine refused that token.");
      setSubmitting(false);
    }
  }
  return (
    <div className="boot" aria-label="Sign in required">
      <WindowDragStrip />
      <div className="boot-brand">
        <Logomark />
        <h1>Tidebreak</h1>
      </div>
      <div className="welcome-copy">
        {failed ? (
          <>
            <h2>{failed.title}</h2>
            <p>{failed.detail}</p>
          </>
        ) : reason === "session_ended" ? (
          <>
            <h2>Your session on this machine ended</h2>
            <p>
              Browser sessions last an hour. Work on this machine keeps running,
              and nothing you did here is lost.
            </p>
          </>
        ) : discovery.mode === "gateway" ? (
          <>
            <h2>Sign in through your Model Gateway console</h2>
            <p>
              This machine runs Tidebreak for your organization. Its address
              alone does not sign you in.
            </p>
          </>
        ) : discovery.mode === "static_token" ? (
          <>
            <h2>Sign in to this machine</h2>
            <p>Use the token your administrator gave you.</p>
          </>
        ) : discovery.mode === "oidc" ? (
          <>
            <h2>Sign in to this machine</h2>
            <p>Continue through your organization&apos;s identity provider.</p>
          </>
        ) : (
          <>
            <h2>This machine is not ready for browser sign-in</h2>
            <p>Ask the administrator to check its authentication settings.</p>
          </>
        )}
        {gatewayUrl ? (
          <p>
            Open the console. It signs you in and brings you straight back to
            this page.
          </p>
        ) : discovery.mode === "static_token" ? (
          <p>
            Paste the token your administrator gave you. This tab keeps it only
            in memory.
          </p>
        ) : discovery.mode === "oidc" ? (
          <p>
            Your identity provider signs you in and brings you straight back to
            this page.
          </p>
        ) : null}
      </div>
      <p className="text-muted-foreground text-sm">
        Machine <code className="font-medium">{machineUrl}</code>
      </p>
      {discovery.mode === "static_token" && (
        <form className="welcome-copy" onSubmit={submitToken}>
          <label className="text-sm font-medium" htmlFor="hosted-token">
            Token
          </label>
          <Input
            id="hosted-token"
            type="password"
            autoComplete="off"
            value={token}
            onChange={(event) => setToken(event.target.value)}
          />
          {tokenError && (
            <p className="text-critical text-sm" role="alert">
              {tokenError}
            </p>
          )}
          <Button
            size="sm"
            type="submit"
            disabled={submitting || !token.trim()}
          >
            {submitting ? "Signing in…" : "Sign in"}
          </Button>
        </form>
      )}
      <div className="boot-actions">
        {gatewayUrl && (
          <Button size="sm" asChild>
            <a href={consoleSignInUrl(gatewayUrl)} rel="noreferrer">
              <ExternalLink size={16} aria-hidden />
              Open the console
            </a>
          </Button>
        )}
        {discovery.mode === "oidc" && (
          <Button size="sm" asChild>
            <a href={oidcSignInUrl(discovery.start_url)}>
              <ExternalLink size={16} aria-hidden />
              Sign in with {discovery.issuer_name}
            </a>
          </Button>
        )}
        <Button
          size="sm"
          variant={
            gatewayUrl ||
            discovery.mode === "oidc" ||
            discovery.mode === "static_token"
              ? "outline"
              : "default"
          }
          onClick={onRetry}
        >
          <RotateCw size={16} aria-hidden />
          Try again
        </Button>
      </div>
    </div>
  );
}
