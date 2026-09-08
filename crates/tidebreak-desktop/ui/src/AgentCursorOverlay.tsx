import { MousePointer2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { STATUS_MARK } from "@/code/statusTone";
import {
  actionMatchesTarget,
  isVisibleComputerUseAction,
  type ComputerUseAction,
  type ComputerUseActionTarget,
} from "./computerUseAction";

export type AgentCursorPreview = ComputerUseActionTarget & {
  coordinateFrame: "viewport" | "window";
  width: number;
  height: number;
};

/** Map only a matching capture or browser viewport. Screen coordinates are never guessed. */
export function agentCursorPosition(
  action: ComputerUseAction,
  preview: AgentCursorPreview,
) {
  const { point, viewport } = action;
  if (
    !actionMatchesTarget(action, preview) ||
    action.coordinateFrame !== preview.coordinateFrame ||
    !point ||
    !viewport ||
    viewport.width !== preview.width ||
    viewport.height !== preview.height ||
    !Number.isFinite(point.x) ||
    !Number.isFinite(point.y) ||
    !Number.isFinite(viewport.width) ||
    !Number.isFinite(viewport.height) ||
    viewport.width <= 0 ||
    viewport.height <= 0 ||
    point.x < 0 ||
    point.y < 0 ||
    point.x >= viewport.width ||
    point.y >= viewport.height
  )
    return null;
  if (action.source === "native" && (!preview.windowId || !preview.captureId))
    return null;
  if (
    action.source !== "native" &&
    (!preview.browserId || preview.documentEpoch === undefined)
  )
    return null;
  return {
    left: `${(point.x / viewport.width) * 100}%`,
    top: `${(point.y / viewport.height) * 100}%`,
  };
}

/** Visual feedback only. The overlay never focuses an element or handles input. */
export function AgentCursorOverlay({
  action,
  preview,
  now = Date.now(),
}: {
  action: ComputerUseAction | null;
  preview: AgentCursorPreview;
  now?: number;
}) {
  if (
    !isVisibleComputerUseAction(action, now) ||
    (action.phase !== "running" && action.phase !== "completed")
  )
    return null;
  const position = agentCursorPosition(action, preview);
  if (!position) return null;
  const running = action.phase === "running";
  return (
    <div
      aria-hidden="true"
      data-agent-cursor-overlay=""
      className="pointer-events-none absolute inset-0 z-20 overflow-hidden select-none"
    >
      <div
        data-agent-cursor=""
        className="pointer-events-none absolute motion-safe:transition-[left,top] motion-safe:duration-150"
        style={position}
      >
        <span
          className={cn(
            "pointer-events-none absolute -left-3 -top-3 size-6 rounded-full border border-current opacity-35",
            running ? STATUS_MARK.running : STATUS_MARK.neutral,
          )}
        />
        <MousePointer2
          aria-hidden
          className={cn(
            "absolute -left-1 -top-1 size-6 fill-current stroke-background",
            running ? STATUS_MARK.running : STATUS_MARK.neutral,
          )}
          strokeWidth={1.6}
        />
      </div>
    </div>
  );
}
