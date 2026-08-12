import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
import {
  resolveComputerUseConfirmation,
  resolveComputerUseConsent,
  resumeComputerUseControl,
  stopComputerUseControl,
  useComputerUseState,
  type ComputerUseCapability,
} from "./computerUse";

/** How a consent ask reads for each capability. */
function capabilityAsk(capability: ComputerUseCapability, app: string): string {
  switch (capability) {
    case "control_app":
      return `Allow OpenWave to control ${app}? It will be able to click, type, and press keys there.`;
    case "capture_screen":
      return `Allow OpenWave to capture ${app} on screen?`;
    case "read_app_content":
      return `Allow OpenWave to read ${app}'s on-screen content?`;
  }
}

function appLabel(appName: string | null, bundleId: string): string {
  // A screen-scoped ask (whole-display capture, screen-wide window list)
  // carries no bundle id.
  if (bundleId === "") return "the whole screen";
  return appName && appName.length > 0 ? appName : bundleId;
}

/**
 * The always-on computer-use surface: a banner while OpenWave is driving an
 * app (with a Stop that halts before the next action), a stopped state with
 * Resume, and the parked consent / confirmation asks the agent is waiting on.
 *
 * Rendered in the shell so control is visible and stoppable from any screen.
 */
export function ComputerUseIndicator() {
  const snapshot = useComputerUseState();
  // The active banner re-arms to hidden once control has been idle past its
  // window; the tick keeps that honest without a native timer.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 5_000);
    return () => clearInterval(timer);
  }, []);

  const showActive =
    snapshot.active !== null && now < snapshot.active.visibleUntilMillis;
  if (
    !snapshot.halted &&
    !showActive &&
    snapshot.pendingConsents.length === 0 &&
    snapshot.pendingConfirmations.length === 0
  ) {
    return null;
  }

  return (
    <div className="bg-background flex flex-col gap-2 border-b px-4 py-2">
      {snapshot.halted ? (
        <div
          className="flex items-center justify-between gap-3 text-sm"
          role="status"
        >
          <span>Computer control is stopped.</span>
          <Button size="sm" variant="outline" onClick={() => void resumeComputerUseControl()}>
            Resume
          </Button>
        </div>
      ) : (
        showActive &&
        snapshot.active && (
          <div
            className="flex items-center justify-between gap-3 text-sm"
            role="status"
          >
            <span>
              OpenWave is controlling{" "}
              {appLabel(snapshot.active.appName, snapshot.active.bundleId)}
            </span>
            <Button
              size="sm"
              variant="outline"
              onClick={() => void stopComputerUseControl()}
            >
              Stop
            </Button>
          </div>
        )
      )}
      {snapshot.pendingConsents.map((prompt) => (
        <section
          key={prompt.callId}
          className="flex max-w-prose flex-col gap-2 rounded-lg border p-3"
          aria-label="Computer use permission"
        >
          <p className="text-sm font-medium break-words">
            {capabilityAsk(
              prompt.capability,
              appLabel(prompt.appName, prompt.bundleId),
            )}
          </p>
          <div className="flex flex-wrap gap-2">
            <Button
              size="sm"
              onClick={() =>
                void resolveComputerUseConsent(prompt.callId, "once")
              }
            >
              Once
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() =>
                void resolveComputerUseConsent(prompt.callId, "chat")
              }
            >
              Always for this chat
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() =>
                void resolveComputerUseConsent(prompt.callId, "always")
              }
            >
              Always
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={() =>
                void resolveComputerUseConsent(prompt.callId, "decline")
              }
            >
              Decline
            </Button>
          </div>
        </section>
      ))}
      {snapshot.pendingConfirmations.map((prompt) => (
        <section
          key={prompt.callId}
          className="flex max-w-prose flex-col gap-2 rounded-lg border p-3"
          aria-label="Confirm action"
        >
          <p className="text-sm font-medium break-words">
            OpenWave wants to {prompt.reason}
            {prompt.targetLabel ? ` — “${prompt.targetLabel}”` : ""} in{" "}
            {appLabel(prompt.appName, prompt.bundleId)}.
          </p>
          <div className="flex flex-wrap gap-2">
            <Button
              size="sm"
              onClick={() =>
                void resolveComputerUseConfirmation(prompt.callId, true)
              }
            >
              Confirm
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={() =>
                void resolveComputerUseConfirmation(prompt.callId, false)
              }
            >
              Deny
            </Button>
          </div>
        </section>
      ))}
    </div>
  );
}
