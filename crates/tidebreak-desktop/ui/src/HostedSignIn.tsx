import { ExternalLink, RotateCw } from "lucide-react";
import { type FormEvent, useState } from "react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  type AuthDiscovery,
  type HandoffFailure,
  consoleSignInUrl,
  oidcSignInUrl,
} from "./hostedSession";
import { Logomark } from "./Logomark";
import { WindowDragStrip } from "./WindowDragStrip";

/**
 * Why a hosted browser tab has no session to show the app with.
 *
 * `no_session`: the page opened without a bearer — someone typed the
 * machine's address, or reloaded a tab. `session_ended`: the bearer the page
 * held stopped being accepted, which after an hour is simply its lifetime.
 * `handoff_failed`: the page came from the machine's landing route and the
 * route could not turn a sign-in into a bearer; `failure` says why.
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
   * How this machine signs a browser in, from its own discovery document.
   * It decides which of the paths below the screen offers.
   */
  discovery: AuthDiscovery;
  /**
   * Probe a pasted token against the machine and, when it holds, keep it for
   * this tab. `false` is the machine refusing it, which stays on this screen.
   */
  onToken?: (token: string) => Promise<boolean>;
  onRetry?: () => void;
};

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
        title: "This machine could not reach the provider that signs you in",
        detail:
          "Sign-in needs that provider to answer, and it did not. Nothing about your account has changed.",
      };
    default:
      return {
        title: "That sign-in did not complete",
        detail:
          "The answer this machine got back was refused, or it was changed on the way here.",
      };
  }
}

/** The heading and the sentence under it: why this screen is here at all. */
function reasonCopy(
  reason: HostedSignInReason,
  failure: HandoffFailure | null,
  discovery: AuthDiscovery,
): { title: string; detail: string } {
  if (reason === "handoff_failed") return handoffFailureCopy(failure);
  if (reason === "session_ended") {
    return {
      title: "Your session on this machine ended",
      detail:
        "Browser sessions last an hour. Work on this machine keeps running, and nothing you did here is lost.",
    };
  }
  if (discovery.mode === "gateway") {
    return {
      title: "Sign in through your Model Gateway console",
      detail:
        "This machine runs Tidebreak for your organization. Its address alone does not sign you in.",
    };
  }
  if (discovery.mode === "static_token" || discovery.mode === "oidc") {
    return {
      title: "Sign in to this machine",
      detail:
        "This machine runs Tidebreak for your organization. Its address alone does not sign you in.",
    };
  }
  return {
    title: "This machine does not sign browsers in",
    detail: "Attach to it from the Tidebreak desktop app instead.",
  };
}

/** What happens once the reader acts, worded for this machine's way in. */
function nextStepCopy(discovery: AuthDiscovery): string | null {
  switch (discovery.mode) {
    case "gateway":
      return "Open the console. It signs you in and brings you straight back to this page.";
    case "static_token":
      return "Paste the token your administrator gave you. This tab keeps it in memory alone and forgets it when you reload.";
    case "oidc":
      return `${discovery.issuer_name} signs you in and brings you straight back to this page.`;
    default:
      return null;
  }
}

/**
 * The screen a hosted browser tab shows until it holds a session.
 *
 * Which path it offers is the machine's to say, not the page's: a gateway
 * machine sends the reader through the console that mints its bearers, a
 * token-file machine takes the token their administrator gave them, and an
 * OIDC machine starts the flow on the machine itself
 * (`docs/decisions/0087-standalone-browser-sign-in.md`). Every path that
 * works leaves the bearer in this tab's memory and nowhere else.
 */
export function HostedSignIn({
  reason,
  failure = null,
  machineUrl,
  discovery,
  onToken,
  onRetry = () => window.location.reload(),
}: HostedSignInProps) {
  const [token, setToken] = useState("");
  const [refused, setRefused] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const { title, detail } = reasonCopy(reason, failure, discovery);
  const nextStep = nextStepCopy(discovery);
  const gatewayUrl =
    discovery.mode === "gateway" ? discovery.gateway_url : null;
  const takesToken = discovery.mode === "static_token" && onToken !== undefined;

  async function submitToken(event: FormEvent) {
    event.preventDefault();
    const pasted = token.trim();
    if (!onToken || !pasted || submitting) return;
    setSubmitting(true);
    setRefused(false);
    // A held token unmounts this screen when boot runs again, so only the
    // refusal has anything left to say.
    const accepted = await onToken(pasted).catch(() => false);
    if (accepted) return;
    setRefused(true);
    setSubmitting(false);
  }

  return (
    <div className="boot" aria-label="Sign in required">
      <WindowDragStrip />
      <div className="boot-brand">
        <Logomark />
        <h1>Tidebreak</h1>
      </div>
      <div className="welcome-copy">
        <h2>{title}</h2>
        <p>{detail}</p>
        {nextStep && <p>{nextStep}</p>}
      </div>
      <p className="text-muted-foreground text-sm">
        Machine <code className="font-medium">{machineUrl}</code>
      </p>
      {takesToken && (
        <form className="boot-token-form" onSubmit={submitToken}>
          <label className="text-sm font-medium" htmlFor="hosted-token">
            Token
          </label>
          <Input
            id="hosted-token"
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={token}
            onChange={(event) => setToken(event.target.value)}
          />
          {refused && (
            <p className="text-critical text-sm" role="alert">
              This machine refused that token. Check it with whoever runs the
              machine, then try again.
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
            gatewayUrl || discovery.mode === "oidc" ? "outline" : "default"
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
