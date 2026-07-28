import { useState, type ReactNode } from "react";

import { PanelPrimaryHeader } from "@/components/PanelHeader";
import { PopoverCollisionBoundaryProvider } from "@/components/ui/popover";
import { usePanelNav } from "./usePanelNav";
import type { PanelPosition } from "./panelTypes";

/**
 * The chrome every panel wears, and the box it lives in.
 *
 * The frame is also the collision boundary for popovers opened inside it, so a
 * menu near the edge of a narrow panel stays within its own panel instead of
 * flipping out over the one beside it.
 */
export function PanelFrame({
  position,
  breadcrumb,
  headerRightSlot,
  spaceBetween,
  showBorder,
  children,
}: {
  position: PanelPosition;
  breadcrumb?: ReactNode;
  headerRightSlot?: ReactNode;
  spaceBetween?: boolean;
  showBorder?: boolean;
  children: ReactNode;
}) {
  const { layout, closePanel, toggleFullscreen } = usePanelNav();
  const [boundary, setBoundary] = useState<HTMLElement | null>(null);
  const isFullscreen = layout.mode === "split" && layout.fullscreen === position;

  return (
    <PopoverCollisionBoundaryProvider boundary={boundary}>
      <div ref={setBoundary} className="flex h-full w-full min-w-0 flex-1 flex-col overflow-clip">
        <PanelPrimaryHeader
          breadcrumb={breadcrumb}
          rightSlot={headerRightSlot}
          spaceBetween={spaceBetween}
          showBorder={showBorder}
          isFullscreen={isFullscreen}
          onToggleFullscreen={() => toggleFullscreen(position)}
          onClose={() => closePanel(position)}
        />
        {children}
      </div>
    </PopoverCollisionBoundaryProvider>
  );
}
