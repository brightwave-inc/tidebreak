import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import {
  resolveComputerUseConfirmation,
  resolveComputerUseConsent,
  resumeComputerUseControl,
  stopComputerUseControl,
  useComputerUseState,
  type ComputerUseConsentPrompt,
} from "./computerUse";

/** Human copy for one broker capability ask. */
function consentCopy(
  prompt: Pick<
    ComputerUseConsentPrompt,
    "appName" | "bundleId" | "capability"
  >,
): { title: string; detail: string | null } {
  if (prompt.bundleId === "") {
    return {
      title: "Allow Tidebreak to capture your entire screen?",
      detail: "This lets the agent see everything visible on the current display.",
    };
  }
  const app = appLabel(prompt.appName, prompt.bundleId);
  switch (prompt.capability) {
    case "control_app":
      return {
        title: `Allow Tidebreak to control ${app}?`,
        detail: `It can click, type, and press keys in ${prompt.bundleId}.`,
      };
    case "capture_screen":
      return {
        title: `Allow Tidebreak to capture ${app}?`,
        detail: `It can take screenshots of ${prompt.bundleId}.`,
      };
    case "read_app_content":
      return {
        title: `Allow Tidebreak to read ${app}?`,
        detail: `It can read the on-screen content exposed by ${prompt.bundleId}.`,
      };
  }
}

function appLabel(appName: string | null, bundleId: string): string {
  // A screen-scoped ask (whole-display capture, screen-wide window list)
  // carries no bundle id.
  if (bundleId === "") return "the whole screen";
  return appName && appName.length > 0 ? appName : bundleId;
}

/**
 * The computer-use HUD: compact permission cards at the top of the window and
 * a small control indicator at the bottom. Both float over the shell instead
 * of resizing every route around a short-lived decision.
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
    <>
      <div className="pointer-events-none fixed inset-x-0 top-5 z-50 flex max-h-[calc(100vh-8rem)] justify-center px-4">
        <div className="flex w-full max-w-[26rem] flex-col gap-2 overflow-y-auto p-1">
          {snapshot.pendingConsents.map((prompt) => {
            const key = `consent:${prompt.callId}`;
            const deciding = busy.has(key);
            const copy = consentCopy(prompt);
            return (
              <section
                key={prompt.callId}
                className="bg-popover text-popover-foreground pointer-events-auto flex flex-col gap-3 rounded-2xl border p-4 shadow-2xl"
                aria-label="Computer use permission"
                aria-busy={deciding}
              >
                <div>
                  <p className="text-muted-foreground mb-1 text-[0.6875rem] font-semibold tracking-[0.08em] uppercase">
                    Computer use
                  </p>
                  <h2 className="text-sm font-semibold break-words">
                    {copy.title}
                  </h2>
                  {copy.detail && (
                    <p className="text-muted-foreground mt-1 text-xs leading-relaxed break-words">
                      {copy.detail}
                    </p>
                  )}
                </div>
                <div className="flex flex-wrap justify-end gap-2">
                  <Button
                    size="xs"
                    variant="ghost"
                    disabled={deciding}
                    onClick={() =>
                      run(key, "Could not send your decision", () =>
                        resolveComputerUseConsent(prompt.callId, "decline"),
                      )
                    }
                  >
                    Don&apos;t allow
                  </Button>
                  <Button
                    size="xs"
                    variant="outline"
                    disabled={deciding}
                    onClick={() =>
                      run(key, "Could not send your decision", () =>
                        resolveComputerUseConsent(prompt.callId, "chat"),
                      )
                    }
                  >
                    Allow for this chat
                  </Button>
                  {prompt.grantScope === "project" && (
                    <Button
                      size="xs"
                      variant="outline"
                      disabled={deciding}
                      onClick={() =>
                        run(key, "Could not send your decision", () =>
                          resolveComputerUseConsent(prompt.callId, "always"),
                        )
                      }
                    >
                      Always in this project
                    </Button>
                  )}
                  <Button
                    size="xs"
                    disabled={deciding}
                    onClick={() =>
                      run(key, "Could not send your decision", () =>
                        resolveComputerUseConsent(prompt.callId, "once"),
                      )
                    }
                  >
                    Allow once
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
                className="bg-popover text-popover-foreground pointer-events-auto flex flex-col gap-3 rounded-2xl border p-4 shadow-2xl"
                aria-label="Confirm action"
                aria-busy={deciding}
              >
                <div>
                  <p className="text-muted-foreground mb-1 text-[0.6875rem] font-semibold tracking-[0.08em] uppercase">
                    Confirm action
                  </p>
                  <h2 className="text-sm font-semibold break-words">
                    Allow Tidebreak to {prompt.reason}?
                  </h2>
                  <p className="text-muted-foreground mt-1 text-xs leading-relaxed break-words">
                    {prompt.targetLabel ? `“${prompt.targetLabel}” in ` : "In "}
                    {appLabel(prompt.appName, prompt.bundleId)}
                  </p>
                </div>
                <div className="flex justify-end gap-2">
                  <Button
                    size="xs"
                    variant="ghost"
                    disabled={deciding}
                    onClick={() =>
                      run(key, "Could not send your decision", () =>
                        resolveComputerUseConfirmation(prompt.callId, false),
                      )
                    }
                  >
                    Don&apos;t allow
                  </Button>
                  <Button
                    size="xs"
                    disabled={deciding}
                    onClick={() =>
                      run(key, "Could not send your decision", () =>
                        resolveComputerUseConfirmation(prompt.callId, true),
                      )
                    }
                  >
                    Allow action
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
      </div>

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
