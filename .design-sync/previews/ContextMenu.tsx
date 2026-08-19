import { useEffect, useRef } from "react";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "tidebreak-desktop-ui";

/** Radix context menus open only from a contextmenu event; fire one on mount
 * so the card shows the open state. */
export function WorkspaceMenu() {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    el.dispatchEvent(
      new MouseEvent("contextmenu", {
        bubbles: true,
        clientX: r.left + 40,
        clientY: r.top + 24,
      }),
    );
  }, []);
  return (
    <ContextMenu modal={false}>
      <ContextMenuTrigger asChild>
        <div
          ref={ref}
          style={{
            width: 260,
            border: "1px solid var(--border)",
            borderRadius: 8,
            padding: "10px 12px",
            fontSize: 13.5,
          }}
        >
          <div style={{ fontWeight: 500 }}>Tighten retry backoff</div>
          <div style={{ fontFamily: "var(--mono)", fontSize: 11, color: "var(--muted-foreground)" }}>
            tidebreak/tighten-retry-backoff
          </div>
        </div>
      </ContextMenuTrigger>
      <ContextMenuContent>
        <ContextMenuItem>Open workspace</ContextMenuItem>
        <ContextMenuItem>New session</ContextMenuItem>
        <ContextMenuItem>Rename…</ContextMenuItem>
        <ContextMenuSeparator />
        <ContextMenuItem>Copy branch name</ContextMenuItem>
        <ContextMenuItem>Copy worktree path</ContextMenuItem>
        <ContextMenuItem>Open pull request</ContextMenuItem>
        <ContextMenuSeparator />
        <ContextMenuItem variant="destructive">Archive</ContextMenuItem>
      </ContextMenuContent>
    </ContextMenu>
  );
}
