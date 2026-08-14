import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import {
  resumeComputerUseControl,
  stopComputerUseControl,
  useComputerUseState,
} from "./computerUse";

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
  if (!snapshot.halted && !showActive) {
    return null;
  }

  return (
    <>
      {(snapshot.halted || (showActive && snapshot.active)) && (
        <div className="pointer-events-none fixed inset-x-0 bottom-5 z-50 flex justify-center px-4">
          <div
            className="bg-popover text-popover-foreground pointer-events-auto flex min-w-0 max-w-md items-center gap-3 rounded-2xl border px-3 py-2.5 shadow-2xl"
            role="status"
          >
            <span
              className={`size-2.5 shrink-0 rounded-full ${
                snapshot.halted
                  ? "bg-muted-foreground"
                  : "bg-emerald-400 shadow-[0_0_0_4px_rgba(52,211,153,0.18)]"
              }`}
              aria-hidden="true"
            />
            <div className="min-w-0 flex-1">
              <p className="truncate text-xs font-semibold">
                {snapshot.halted
                  ? "Computer control is stopped"
                  : `Tidebreak is controlling ${appLabel(
                      snapshot.active?.appName ?? null,
                      snapshot.active?.bundleId ?? "",
                    )}`}
              </p>
              <p className="text-muted-foreground text-[0.6875rem]">
                {snapshot.halted
                  ? "Resume only when you want the agent to continue."
                  : "You can stop before the next action."}
              </p>
            </div>
            <Button
              size="xs"
              variant={snapshot.halted ? "outline" : "destructive"}
              disabled={busy.has("control")}
              onClick={() =>
                snapshot.halted
                  ? run("control", "Could not resume control", () =>
                      resumeComputerUseControl(),
                    )
                  : run("control", "Could not stop control", () =>
                      stopComputerUseControl(),
                    )
              }
            >
              {snapshot.halted ? "Resume" : busy.has("control") ? "Stopping…" : "Stop"}
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
