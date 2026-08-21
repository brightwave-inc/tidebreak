import { useEffect, useRef, useState } from "react";
import { SquareTerminal, X } from "lucide-react";

import type { ApiClient } from "../api/client";
import { Button } from "@/components/ui/button";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { useCodeUiStore } from "./CodeUiStore";
import { HOVER_TINT } from "./interactive";
import { TerminalPane } from "./TerminalPane";

export const DEFAULT_TERMINAL_DRAWER_HEIGHT = 240;
export const MIN_TERMINAL_DRAWER_HEIGHT = 140;
export const MAX_TERMINAL_DRAWER_HEIGHT = 560;
const HEIGHT_STEP = 32;
const REVEAL_MS = 140;

type RevealPhase = "pre" | "in" | "open" | "out";

/**
 * Auxiliary shell as a bottom drawer under the conversation.
 *
 * Height is remembered per workspace so a reopen matches the last size.
 * Open and close clip the reserved band in 140ms; the xterm host stays at
 * that height so streamed output does not reflow the transcript.
 */
export function TerminalDrawer({
  client,
  workspaceId,
  shortcutHint,
  onClose,
}: {
  client: ApiClient;
  workspaceId: string;
  shortcutHint?: string;
  onClose: () => void;
}) {
  const storedHeight = useCodeUiStore(
    (state) => state.terminalDrawerHeights[workspaceId],
  );
  const setTerminalDrawerHeight = useCodeUiStore(
    (state) => state.setTerminalDrawerHeight,
  );
  const height = clampDrawerHeight(
    storedHeight ?? DEFAULT_TERMINAL_DRAWER_HEIGHT,
  );
  const dragRef = useRef<{ startY: number; startHeight: number } | null>(null);
  const closeTimerRef = useRef<number | null>(null);
  const [phase, setPhase] = useState<RevealPhase>(() =>
    prefersReducedMotion() ? "open" : "pre",
  );

  useEffect(() => {
    function onMove(event: PointerEvent) {
      const drag = dragRef.current;
      if (!drag) return;
      const next = drag.startHeight + (drag.startY - event.clientY);
      setTerminalDrawerHeight(workspaceId, clampDrawerHeight(next));
    }
    function onUp() {
      dragRef.current = null;
    }
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
  }, [setTerminalDrawerHeight, workspaceId]);

  useEffect(() => {
    if (phase !== "pre") return;
    const frame = requestAnimationFrame(() => setPhase("in"));
    return () => cancelAnimationFrame(frame);
  }, [phase]);

  useEffect(() => {
    if (phase !== "in") return;
    const timer = window.setTimeout(() => setPhase("open"), REVEAL_MS);
    return () => window.clearTimeout(timer);
  }, [phase]);

  useEffect(() => {
    return () => {
      if (closeTimerRef.current != null) {
        window.clearTimeout(closeTimerRef.current);
      }
    };
  }, []);

  function applyHeight(next: number) {
    setTerminalDrawerHeight(workspaceId, clampDrawerHeight(next));
  }

  function hide() {
    if (prefersReducedMotion() || phase === "pre") {
      onClose();
      return;
    }
    setPhase("out");
    closeTimerRef.current = window.setTimeout(onClose, REVEAL_MS);
  }

  const expanded = phase === "in" || phase === "open";
  const transitioning = phase !== "open";

  return (
    <aside
      className={cn(
        "shrink-0 overflow-hidden border-t",
        transitioning &&
          "transition-[max-height] duration-[140ms] ease-out motion-reduce:transition-none",
      )}
      style={{ maxHeight: expanded ? height : 0 }}
      aria-label="Terminal"
      data-testid="terminal-drawer"
    >
      <div className="flex flex-col overflow-hidden" style={{ height }}>
        <div
          role="separator"
          tabIndex={0}
          aria-orientation="horizontal"
          aria-label="Resize terminal"
          aria-valuemin={MIN_TERMINAL_DRAWER_HEIGHT}
          aria-valuemax={MAX_TERMINAL_DRAWER_HEIGHT}
          aria-valuenow={height}
          className={cn(
            "hover:bg-muted focus-visible:bg-muted flex h-1.5 shrink-0 cursor-ns-resize items-center justify-center focus-visible:outline-none",
            HOVER_TINT,
          )}
          onPointerDown={(event) => {
            event.preventDefault();
            dragRef.current = { startY: event.clientY, startHeight: height };
          }}
          onKeyDown={(event) => {
            if (event.key === "ArrowUp") {
              event.preventDefault();
              applyHeight(height + HEIGHT_STEP);
            } else if (event.key === "ArrowDown") {
              event.preventDefault();
              applyHeight(height - HEIGHT_STEP);
            }
          }}
        >
          <span className="bg-border h-0.5 w-8 rounded-full" />
        </div>
        <header className="flex h-8 shrink-0 items-center gap-2 px-3">
          <SquareTerminal className="text-muted-foreground size-3.5 shrink-0" />
          <h2 className="min-w-0 flex-1 truncate text-sm font-medium">
            Terminal
          </h2>
          <WithTooltip
            label={
              shortcutHint ? `Hide terminal ${shortcutHint}` : "Hide terminal"
            }
          >
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label="Hide terminal"
              onClick={hide}
            >
              <X />
            </Button>
          </WithTooltip>
        </header>
        <TerminalPane client={client} workspaceId={workspaceId} hideHeader />
      </div>
    </aside>
  );
}

export function clampDrawerHeight(value: number): number {
  return Math.min(
    MAX_TERMINAL_DRAWER_HEIGHT,
    Math.max(MIN_TERMINAL_DRAWER_HEIGHT, Math.round(value)),
  );
}

function prefersReducedMotion(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}
