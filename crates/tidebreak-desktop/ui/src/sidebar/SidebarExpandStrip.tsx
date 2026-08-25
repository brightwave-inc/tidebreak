import { PanelLeftOpen } from "lucide-react";

import { WithTooltip } from "@/components/ui/tooltip";
import { useUiStore } from "@/UiStore";

/**
 * The way back when the rail is collapsed away.
 *
 * Collapsed means the sidebar is gone from the layout entirely, so the shell
 * owes the reader a control to return it. This row pins one expand button
 * beside the macOS traffic lights (at the plain left edge everywhere else),
 * reserves their titlebar space, and keeps that space draggable.
 */
export function SidebarExpandStrip({ macOverlay }: { macOverlay: boolean }) {
  const collapsed = useUiStore((state) => state.sidebarCollapsed);
  const toggleSidebar = useUiStore((state) => state.toggleSidebar);
  if (!collapsed) return null;

  return (
    <div
      className={`sidebar-expand-strip${macOverlay ? " is-mac-overlay" : ""}`}
      {...(macOverlay ? { "data-tauri-drag-region": true } : {})}
    >
      <WithTooltip label="Expand sidebar" side="bottom">
        <button
          type="button"
          className="inline-flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-md text-muted-foreground hover:bg-[color-mix(in_srgb,var(--ink)_8%,transparent)] hover:text-foreground"
          aria-label="Expand sidebar"
          onClick={toggleSidebar}
        >
          <PanelLeftOpen size={16} />
        </button>
      </WithTooltip>
    </div>
  );
}
