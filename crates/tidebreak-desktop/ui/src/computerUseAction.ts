import { isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

export const COMPUTER_USE_ACTION_EVENT = "computer-use-action";

export type ComputerUseAction = {
  actionId: string;
  sessionId: string;
  source: "browser" | "native" | "chrome";
  action:
    | "click"
    | "double_click"
    | "type"
    | "key"
    | "scroll"
    | "drag"
    | "move";
  phase:
    | "running"
    | "completed"
    | "failed"
    | "foreground_required"
    | "cancelled";
  executionMode: "background" | "foreground";
  coordinateFrame: "viewport" | "window" | "screen";
  startedAtMillis: number;
  visibleUntilMillis: number;
  point?: { x: number; y: number };
  viewport?: { width: number; height: number };
  targetBounds?: { x: number; y: number; width: number; height: number };
  browserId?: string;
  workspaceId?: string;
  instanceId?: string;
  documentEpoch?: number;
  bundleId?: string;
  windowId?: number;
  captureId?: string;
};

export type ComputerUseActionTarget = Partial<
  Pick<
    ComputerUseAction,
    | "source"
    | "sessionId"
    | "browserId"
    | "workspaceId"
    | "instanceId"
    | "documentEpoch"
    | "bundleId"
    | "windowId"
    | "captureId"
  >
> & { source: ComputerUseAction["source"] };

/** Exact host identity prevents a late action from marking a different view. */
export function actionMatchesTarget(
  action: ComputerUseAction,
  target: ComputerUseActionTarget,
): boolean {
  const keys = [
    "source",
    "sessionId",
    "browserId",
    "workspaceId",
    "instanceId",
    "documentEpoch",
    "bundleId",
    "windowId",
    "captureId",
  ] as const;
  return keys.every(
    (key) => target[key] === undefined || action[key] === target[key],
  );
}

export function isVisibleComputerUseAction(
  action: ComputerUseAction | null,
  now = Date.now(),
): action is ComputerUseAction {
  return (
    action !== null &&
    action.phase !== "cancelled" &&
    Number.isFinite(action.visibleUntilMillis) &&
    action.visibleUntilMillis > now
  );
}

/** Action events are transient. Expired and older events cannot revive a cursor. */
export function useComputerUseAction(
  target?: ComputerUseActionTarget,
  sources?: readonly ComputerUseAction["source"][],
): ComputerUseAction | null {
  const [action, setAction] = useState<ComputerUseAction | null>(null);
  const targetKey = JSON.stringify({
    target: target ?? null,
    sources: sources ?? null,
  });
  useEffect(() => {
    setAction(null);
    if (!isTauri()) return;
    const { target: selected, sources: selectedSources } = JSON.parse(
      targetKey,
    ) as {
      target: ComputerUseActionTarget | null;
      sources: ComputerUseAction["source"][] | null;
    };
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    let latest: ComputerUseAction | null = null;
    void listen<ComputerUseAction>(COMPUTER_USE_ACTION_EVENT, ({ payload }) => {
      if (
        cancelled ||
        (selectedSources && !selectedSources.includes(payload.source)) ||
        (selected && !actionMatchesTarget(payload, selected))
      )
        return;
      if (!Number.isFinite(payload.startedAtMillis)) return;
      if (
        latest &&
        (payload.startedAtMillis < latest.startedAtMillis ||
          (payload.actionId === latest.actionId &&
            latest.phase !== "running" &&
            payload.phase === "running"))
      )
        return;
      latest = payload;
      setAction(isVisibleComputerUseAction(payload) ? payload : null);
    })
      .then((stop) => {
        if (cancelled) stop();
        else unlisten = stop;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [targetKey]);
  useEffect(() => {
    if (!action) return;
    const timeout = window.setTimeout(
      () => setAction((current) => (current === action ? null : current)),
      Math.max(0, action.visibleUntilMillis - Date.now()),
    );
    return () => window.clearTimeout(timeout);
  }, [action]);
  return isVisibleComputerUseAction(action) &&
    (!target || actionMatchesTarget(action, target))
    ? action
    : null;
}

export function computerUseActionLabel(action: ComputerUseAction): string {
  if (action.phase === "foreground_required") return "Needs foreground access";
  if (action.phase === "failed") return "Action failed";
  if (action.phase === "cancelled") return "Action stopped";
  const labels: Record<ComputerUseAction["action"], string> = {
    click: "Clicking",
    double_click: "Double-clicking",
    type: "Typing",
    key: "Pressing a key",
    scroll: "Scrolling",
    drag: "Dragging",
    move: "Moving to a target",
  };
  return action.phase === "completed"
    ? "Action completed"
    : labels[action.action];
}

export function computerUseModeLabel(action: ComputerUseAction): string {
  if (action.phase === "foreground_required")
    return "Waiting for permission to use the foreground";
  if (action.phase === "completed")
    return action.executionMode === "background"
      ? "Ran in the background"
      : "Ran in the foreground";
  if (action.phase === "failed")
    return action.executionMode === "background"
      ? "Background action"
      : "Foreground action";
  return action.executionMode === "background"
    ? "Working in the background"
    : "Using the foreground";
}
