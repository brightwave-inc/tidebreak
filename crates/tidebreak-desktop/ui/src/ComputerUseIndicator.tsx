import { useEffect, useRef, useState } from "react";
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
      return `Allow Tidebreak to control ${app}? It will be able to click, type, and press keys there.`;
    case "capture_screen":
      return `Allow Tidebreak to capture ${app} on screen?`;
    case "read_app_content":
      return `Allow Tidebreak to read ${app}'s on-screen content?`;
  }
}

function appLabel(appName: string | null, bundleId: string): string {
  // A screen-scoped ask (whole-display capture, screen-wide window list)
  // carries no bundle id.
  if (bundleId === "") return "the whole screen";
  return appName && appName.length > 0 ? appName : bundleId;
}

function consentLabel(appName: string | null, bundleId: string): string {
  if (bundleId === "") return "the whole screen";
  // The bundle id is the principal the grant is actually written for; an app's
  // self-reported name is attacker-influenceable, so the consent question leads
  // with the bundle id and shows the name only as a parenthetical.
  return appName && appName.length > 0 ? `${bundleId} (${appName})` : bundleId;
}

/**
 * The always-on computer-use surface: a banner while Tidebreak is driving an
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

  // In-flight invokes, keyed per card (or "control" for Stop/Resume). The
  // ref is the guard against a second click landing while the first is
  // pending; the state mirrors it so the buttons can disable. A failure
  // stays on the card that raised it, the way ApprovalCard surfaces errors.
  const [busy, setBusy] = useState<ReadonlySet<string>>(new Set());
  const busyRef = useRef(new Set<string>());
  const [errors, setErrors] = useState<Record<string, string>>({});

  function run(key: string, failure: string, action: () => Promise<void>) {
    if (busyRef.current.has(key)) return;
    busyRef.current.add(key);
    setBusy(new Set(busyRef.current));
    setErrors((current) => {
      const next = { ...current };
      delete next[key];
      return next;
    });
    void action()
      .catch((err: unknown) => {
        setErrors((current) => ({
          ...current,
          [key]: `${failure}: ${String(err)}`,
        }));
      })
      .finally(() => {
        busyRef.current.delete(key);
        setBusy(new Set(busyRef.current));
      });
  }

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
          <Button
            size="sm"
            variant="outline"
            disabled={busy.has("control")}
            onClick={() =>
              run("control", "Could not resume control", () =>
                resumeComputerUseControl(),
              )
            }
          >
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
              Tidebreak is controlling{" "}
              {appLabel(snapshot.active.appName, snapshot.active.bundleId)}
            </span>
            <Button
              size="sm"
              variant="outline"
              disabled={busy.has("control")}
              onClick={() =>
                run("control", "Could not stop control", () =>
                  stopComputerUseControl(),
                )
              }
            >
              Stop
            </Button>
          </div>
        )
      )}
      {errors.control && (
        <p className="text-destructive text-xs break-words" role="alert">
          {errors.control}
        </p>
      )}
      {snapshot.pendingConsents.map((prompt) => {
        const key = `consent:${prompt.callId}`;
        const deciding = busy.has(key);
        return (
          <section
            key={prompt.callId}
            className="flex max-w-prose flex-col gap-2 rounded-lg border p-3"
            aria-label="Computer use permission"
            aria-busy={deciding}
          >
            <p className="text-sm font-medium break-words">
              {capabilityAsk(
                prompt.capability,
                consentLabel(prompt.appName, prompt.bundleId),
              )}
            </p>
            <div className="flex flex-wrap gap-2">
              <Button
                size="sm"
                disabled={deciding}
                onClick={() =>
                  run(key, "Could not send your decision", () =>
                    resolveComputerUseConsent(prompt.callId, "once"),
                  )
                }
              >
                Once
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={deciding}
                onClick={() =>
                  run(key, "Could not send your decision", () =>
                    resolveComputerUseConsent(prompt.callId, "chat"),
                  )
                }
              >
                Always for this chat
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={deciding}
                onClick={() =>
                  run(key, "Could not send your decision", () =>
                    resolveComputerUseConsent(prompt.callId, "always"),
                  )
                }
              >
                Always
              </Button>
              <Button
                size="sm"
                variant="ghost"
                disabled={deciding}
                onClick={() =>
                  run(key, "Could not send your decision", () =>
                    resolveComputerUseConsent(prompt.callId, "decline"),
                  )
                }
              >
                Decline
              </Button>
            </div>
            {errors[key] && (
              <p className="text-destructive text-xs break-words" role="alert">
                {errors[key]}
              </p>
            )}
          </section>
        );
      })}
      {snapshot.pendingConfirmations.map((prompt) => {
        const key = `confirmation:${prompt.callId}`;
        const deciding = busy.has(key);
        return (
          <section
            key={prompt.callId}
            className="flex max-w-prose flex-col gap-2 rounded-lg border p-3"
            aria-label="Confirm action"
            aria-busy={deciding}
          >
            <p className="text-sm font-medium break-words">
              Tidebreak wants to {prompt.reason}
              {prompt.targetLabel ? ` — “${prompt.targetLabel}”` : ""} in{" "}
              {appLabel(prompt.appName, prompt.bundleId)}.
            </p>
            <div className="flex flex-wrap gap-2">
              <Button
                size="sm"
                disabled={deciding}
                onClick={() =>
                  run(key, "Could not send your decision", () =>
                    resolveComputerUseConfirmation(prompt.callId, true),
                  )
                }
              >
                Confirm
              </Button>
              <Button
                size="sm"
                variant="ghost"
                disabled={deciding}
                onClick={() =>
                  run(key, "Could not send your decision", () =>
                    resolveComputerUseConfirmation(prompt.callId, false),
                  )
                }
              >
                Deny
              </Button>
            </div>
            {errors[key] && (
              <p className="text-destructive text-xs break-words" role="alert">
                {errors[key]}
              </p>
            )}
          </section>
        );
      })}
    </div>
  );
}
