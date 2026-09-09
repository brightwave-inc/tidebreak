import { useRef, useState } from "react";
import { useNowWhile } from "./BackgroundAgentPanel";
import { Button } from "@/components/ui/button";
import {
  resumeComputerUseControl,
  stopComputerUseControl,
  useComputerUseState,
  type ComputerUseSnapshot,
} from "./computerUse";
import {
  computerUseActionLabel,
  computerUseModeLabel,
  isVisibleComputerUseAction,
  useComputerUseAction,
  type ComputerUseAction,
} from "./computerUseAction";
import { Check, Hand, MousePointer2, TriangleAlert } from "lucide-react";

function appLabel(appName: string | null, bundleId: string): string {
  // A screen-scoped ask (whole-display capture, screen-wide window list)
  // carries no bundle id.
  if (bundleId === "") return "the whole screen";
  return appName && appName.length > 0 ? appName : bundleId;
}

/**
 * The computer-use HUD is only an indicator and emergency stop. Consent and
 * consequential confirmation are native dialogs, outside renderer authority.
 *
 * Rendered in the shell so control is visible and stoppable from any screen.
 */
export function ComputerUseIndicator() {
  const snapshot = useComputerUseState();
  const action = useComputerUseAction(undefined, ["native", "chrome"]);
  return (
    <ComputerUseIndicatorView
      snapshot={snapshot}
      action={action}
      onStop={stopComputerUseControl}
      onResume={resumeComputerUseControl}
    />
  );
}

export function ComputerUseIndicatorView({
  snapshot,
  action = null,
  onStop,
  onResume,
}: {
  snapshot: ComputerUseSnapshot;
  action?: ComputerUseAction | null;
  onStop: () => Promise<void>;
  onResume: () => Promise<void>;
}) {
  const liveAction =
    (action?.source === "native" || action?.source === "chrome") &&
    isVisibleComputerUseAction(action)
      ? action
      : null;
  const foregroundRequired = liveAction?.phase === "foreground_required";
  const failed = liveAction?.phase === "failed";
  const completed = liveAction?.phase === "completed";
  const label =
    liveAction?.source === "chrome"
      ? "Google Chrome"
      : appLabel(
          snapshot.active?.bundleId ===
            (liveAction?.bundleId ?? snapshot.active?.bundleId)
            ? (snapshot.active?.appName ?? null)
            : null,
          liveAction?.bundleId ?? snapshot.active?.bundleId ?? "",
        );
  // The active banner re-arms to hidden once control has been idle past its
  // window; the tick keeps that honest without a native timer. It only runs
  // while the banner is up: with no session on record — the common case for a
  // shell-mounted component — or one already idled out, nothing on screen
  // depends on the time and nothing re-renders. The tick's job is the
  // re-render itself, which re-evaluates this line; the crossing tick renders
  // the banner away and stops the interval with it.
  const showActive =
    Boolean(liveAction) ||
    (snapshot.active !== null &&
      Date.now() < snapshot.active.visibleUntilMillis);
  useNowWhile(showActive);

  const stoppedSessions = Math.max(0, snapshot.stoppedSessions ?? 0);
  const hasStoppedSessions = stoppedSessions > 0;
  const resumeOnly = snapshot.halted || (!showActive && hasStoppedSessions);
  const stoppedLabel =
    stoppedSessions === 1
      ? "Computer control is stopped for 1 session"
      : `Computer control is stopped for ${stoppedSessions} sessions`;
  const [pendingControls, setPendingControls] = useState<
    ReadonlySet<"stop" | "resume">
  >(new Set());
  const busyRef = useRef(new Set<"stop" | "resume">());
  const [error, setError] = useState<string | null>(null);

  function run(control: "stop" | "resume") {
    if (
      busyRef.current.has(control) ||
      (control === "resume" && busyRef.current.size > 0)
    )
      return;
    busyRef.current.add(control);
    setPendingControls(new Set(busyRef.current));
    setError(null);
    void (control === "stop" ? onStop() : onResume())
      .catch((err: unknown) => {
        setError(`Could not ${control} control: ${String(err)}`);
      })
      .finally(() => {
        busyRef.current.delete(control);
        setPendingControls(new Set(busyRef.current));
      });
  }

  if (!snapshot.halted && !showActive && !hasStoppedSessions) {
    return null;
  }

  return (
    <>
      {(snapshot.halted || showActive || hasStoppedSessions) && (
        <div className="pointer-events-none fixed inset-x-0 bottom-5 z-50 flex justify-center px-4">
          <div
            className="bg-popover text-popover-foreground pointer-events-auto flex min-w-0 max-w-md items-center gap-3 rounded-xl border px-3 py-2.5 shadow-lg"
            role="status"
          >
            {resumeOnly ? (
              <MousePointer2
                aria-hidden
                className="size-4 shrink-0 text-muted-foreground"
              />
            ) : foregroundRequired ? (
              <Hand aria-hidden className="size-4 shrink-0 text-warning" />
            ) : failed ? (
              <TriangleAlert
                aria-hidden
                className="size-4 shrink-0 text-critical"
              />
            ) : completed ? (
              <Check
                aria-hidden
                className="size-4 shrink-0 text-muted-foreground"
              />
            ) : (
              <MousePointer2
                aria-hidden
                className="size-4 shrink-0 text-live"
              />
            )}
            <div className="min-w-0 flex-1">
              <p className="truncate text-xs font-semibold">
                {snapshot.halted
                  ? "Computer control is stopped"
                  : resumeOnly
                    ? stoppedLabel
                    : foregroundRequired
                      ? "Tidebreak needs foreground access"
                      : failed
                        ? `Action failed in ${label}`
                        : completed
                          ? `Action completed in ${label}`
                          : liveAction?.phase === "running"
                            ? `Computer use request for ${label}`
                            : `Computer use: ${label}`}
              </p>
              <p className="text-muted-foreground text-2xs">
                {snapshot.halted
                  ? "Resume only when you want the agent to continue."
                  : resumeOnly
                    ? "Resume when you want these sessions to continue."
                    : foregroundRequired && liveAction
                      ? computerUseModeLabel(liveAction)
                      : liveAction
                        ? `${computerUseActionLabel(liveAction)} · ${computerUseModeLabel(liveAction)}`
                        : "You can stop before the next action."}
              </p>
              {!snapshot.halted && !resumeOnly && hasStoppedSessions && (
                <p className="text-muted-foreground text-2xs">
                  {stoppedSessions === 1
                    ? "1 session has stopped computer control."
                    : `${stoppedSessions} sessions have stopped computer control.`}
                </p>
              )}
            </div>
            {!resumeOnly && hasStoppedSessions && (
              <Button
                size="xs"
                variant="outline"
                disabled={pendingControls.size > 0}
                onClick={() => run("resume")}
                aria-label="Resume stopped sessions"
              >
                {pendingControls.has("resume") ? "Resuming…" : "Resume"}
              </Button>
            )}
            <Button
              size="xs"
              variant={resumeOnly ? "outline" : "destructive"}
              disabled={
                resumeOnly
                  ? pendingControls.size > 0
                  : pendingControls.has("stop")
              }
              onClick={() => run(resumeOnly ? "resume" : "stop")}
            >
              {resumeOnly
                ? pendingControls.has("resume")
                  ? "Resuming…"
                  : "Resume"
                : pendingControls.has("stop")
                  ? "Stopping…"
                  : "Stop"}
            </Button>
          </div>
          {error && (
            <p
              className="bg-popover text-destructive pointer-events-auto absolute bottom-full mb-2 rounded-lg border px-3 py-2 text-xs shadow-lg"
              role="alert"
            >
              {error}
            </p>
          )}
        </div>
      )}
    </>
  );
}
