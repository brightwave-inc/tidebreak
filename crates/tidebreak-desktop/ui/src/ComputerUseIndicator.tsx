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

  if (!snapshot.halted && !showActive) {
    return null;
  }

  return (
    <>
      {(snapshot.halted || showActive) && (
        <div className="pointer-events-none fixed inset-x-0 bottom-5 z-50 flex justify-center px-4">
          <div
            className="bg-popover text-popover-foreground pointer-events-auto flex min-w-0 max-w-md items-center gap-3 rounded-xl border px-3 py-2.5 shadow-lg"
            role="status"
          >
            {snapshot.halted ? (
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
                  : foregroundRequired
                    ? "Tidebreak needs foreground access"
                    : failed
                      ? `Action failed in ${label}`
                      : completed
                        ? `Action completed in ${label}`
                        : `Tidebreak is controlling ${label}`}
              </p>
              <p className="text-muted-foreground text-2xs">
                {snapshot.halted
                  ? "Resume only when you want the agent to continue."
                  : foregroundRequired && liveAction
                    ? computerUseModeLabel(liveAction)
                    : liveAction
                      ? `${computerUseActionLabel(liveAction)} · ${computerUseModeLabel(liveAction)}`
                      : "You can stop before the next action."}
              </p>
            </div>
            <Button
              size="xs"
              variant={snapshot.halted ? "outline" : "destructive"}
              disabled={busy.has("control")}
              onClick={() =>
                snapshot.halted
                  ? run("control", "Could not resume control", () => onResume())
                  : run("control", "Could not stop control", () => onStop())
              }
            >
              {snapshot.halted
                ? "Resume"
                : busy.has("control")
                  ? "Stopping…"
                  : "Stop"}
            </Button>
          </div>
          {errors.control && (
            <p
              className="bg-popover text-destructive pointer-events-auto absolute bottom-full mb-2 rounded-lg border px-3 py-2 text-xs shadow-lg"
              role="alert"
            >
              {errors.control}
            </p>
          )}
        </div>
      )}
    </>
  );
}
