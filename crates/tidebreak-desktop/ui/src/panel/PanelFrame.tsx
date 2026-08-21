import { useState, type ReactNode } from "react";

import { PanelPrimaryHeader } from "@/components/PanelHeader";
import { PopoverCollisionBoundaryProvider } from "@/components/ui/popover";
import { usePanelNav } from "./usePanelNav";

/**
 * The chrome every panel wears, and the box it lives in.
 *
 * The frame is also the collision boundary for popovers opened inside it, so a
 * menu near the edge of a narrow panel stays within its own panel instead of
 * flipping out over the conversation beside it.
 *
 * Only the tab showing is mounted, so the close control here is that tab's:
 * closing it hands focus to its neighbour, or back to the conversation when it
 * was the last one open.
 */
export function PanelFrame({
  breadcrumb,
  headerRightSlot,
  spaceBetween,
  showBorder,
  children,
}: {
  breadcrumb?: ReactNode;
  headerRightSlot?: ReactNode;
  spaceBetween?: boolean;
  showBorder?: boolean;
  children: ReactNode;
}) {
  const { layout, closeTab, toggleFullscreen } = usePanelNav();
  const [boundary, setBoundary] = useState<HTMLElement | null>(null);

  return (
    <PopoverCollisionBoundaryProvider boundary={boundary}>
      <div
        ref={setBoundary}
        className="flex h-full w-full min-w-0 flex-1 flex-col overflow-clip"
      >
        <PanelPrimaryHeader
          breadcrumb={breadcrumb}
          rightSlot={headerRightSlot}
          spaceBetween={spaceBetween}
          showBorder={showBorder}
          isFullscreen={layout.fullscreen}
          onToggleFullscreen={() => toggleFullscreen()}
          onClose={() => closeTab()}
        />
        {children}
      </div>
    </PopoverCollisionBoundaryProvider>
  );
}
