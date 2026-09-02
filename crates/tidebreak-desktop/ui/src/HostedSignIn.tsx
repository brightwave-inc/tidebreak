import { ExternalLink, RotateCw } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Logomark } from "./Logomark";
import { WindowDragStrip } from "./WindowDragStrip";

/**
 * Why a hosted browser tab has no session to show the app with.
 *
 * `no_session`: the page opened without a bearer — someone typed the
 * machine's address, or reloaded a tab. `session_ended`: the bearer the page
 * held stopped being accepted, which after an hour is simply its lifetime.
 */
export type HostedSignInReason = "no_session" | "session_ended";

export type HostedSignInProps = {
  reason: HostedSignInReason;
  /** The machine this page is served by. */
  machineUrl: string;
  /**
   * The console that can sign the reader in here. `null` when the machine
   * has no gateway, in which case the page can only say where to go instead.
   */
  gatewayUrl: string | null;
  onRetry?: () => void;
};

/**
 * The screen a hosted browser tab shows until it holds a session.
 *
 * A browser tab cannot start the sign-in itself: the bearer it needs is
 * minted by the reader's Model Gateway console, which hands it to this page
 * once. So the screen names the console and sends the reader there; the
 * console's Manage Tidebreak action is what brings them back signed in.
 */
export function HostedSignIn({
  reason,
  machineUrl,
  gatewayUrl,
  onRetry = () => window.location.reload(),
}: HostedSignInProps) {
  const ended = reason === "session_ended";
  return (
    <div className="boot" aria-label="Sign in required">
      <WindowDragStrip />
      <div className="boot-brand">
        <Logomark />
        <h1>Tidebreak</h1>
      </div>
      <div className="welcome-copy">
        {ended ? (
          <>
            <h2>Your session on this machine ended</h2>
            <p>
              Browser sessions last an hour. Work on this machine keeps running,
              and nothing you did here is lost.
            </p>
          </>
        ) : (
          <>
            <h2>Sign in through your Model Gateway console</h2>
            <p>
              This machine runs Tidebreak for your organization. Its address
              alone does not sign you in.
            </p>
          </>
        )}
        {gatewayUrl ? (
          <p>
            Open the console and choose <strong>Manage Tidebreak</strong> to
            come back here signed in.
          </p>
        ) : (
          <p>
            This machine does not sign browsers in. Attach to it from the
            Tidebreak desktop app instead.
          </p>
        )}
      </div>
      <p className="text-muted-foreground text-sm">
        Machine <code className="font-medium">{machineUrl}</code>
      </p>
      <div className="boot-actions">
        {gatewayUrl && (
          <Button size="sm" asChild>
            <a href={gatewayUrl} rel="noreferrer">
              <ExternalLink size={16} aria-hidden />
              Open the console
            </a>
          </Button>
        )}
        <Button
          size="sm"
          variant={gatewayUrl ? "outline" : "default"}
          onClick={onRetry}
        >
          <RotateCw size={16} aria-hidden />
          Try again
        </Button>
      </div>
    </div>
  );
}
