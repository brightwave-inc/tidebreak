import { useEffect, useRef, useState } from "react";
import { SquareTerminal, X } from "lucide-react";

import type { ApiClient } from "../api/client";
import { Button } from "@/components/ui/button";
import { WithTooltip } from "@/components/ui/tooltip";
import { TerminalPane } from "./TerminalPane";

const DEFAULT_HEIGHT = 240;
const MIN_HEIGHT = 140;
const MAX_HEIGHT = 560;

/**
 * Auxiliary shell as a bottom drawer under the conversation.
 *
 * The PTY is the same ephemeral surface as before; only where it sits
 * changed. Height is local to this mount — a reload starts at the default
 * again, which matches terminals that do not survive a restart.
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
  const [height, setHeight] = useState(DEFAULT_HEIGHT);
  const dragRef = useRef<{ startY: number; startHeight: number } | null>(null);

  useEffect(() => {
    function onMove(event: PointerEvent) {
      const drag = dragRef.current;
      if (!drag) return;
      const next = drag.startHeight + (drag.startY - event.clientY);
      setHeight(Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, next)));
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
  }, []);

  return (
    <aside
      className="flex shrink-0 flex-col overflow-hidden border-t"
      style={{ height }}
      aria-label="Terminal"
      data-testid="terminal-drawer"
    >
      <div
        role="separator"
        aria-orientation="horizontal"
        aria-label="Resize terminal"
        className="hover:bg-muted flex h-1.5 shrink-0 cursor-ns-resize items-center justify-center"
        onPointerDown={(event) => {
          event.preventDefault();
          dragRef.current = { startY: event.clientY, startHeight: height };
        }}
      >
        <span className="bg-border h-0.5 w-8 rounded-full" />
      </div>
      <header className="flex h-8 shrink-0 items-center gap-2 px-3">
        <SquareTerminal className="text-muted-foreground size-3.5 shrink-0" />
        <h2 className="min-w-0 flex-1 truncate text-xs font-medium">Terminal</h2>
        <WithTooltip label={shortcutHint ? `Hide terminal ${shortcutHint}` : "Hide terminal"}>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label="Hide terminal"
            onClick={onClose}
          >
            <X />
          </Button>
        </WithTooltip>
      </header>
      <TerminalPane client={client} workspaceId={workspaceId} hideHeader />
    </aside>
  );
}
