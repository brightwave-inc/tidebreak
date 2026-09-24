import { useEffect, useState } from "react";

import { useUiStore } from "@/UiStore";
import { SIDEBAR_OVERLAY_MAX_WIDTH, sidebarUsesOverlay } from "./sidebarLayout";

function readViewportWidth(): number {
  if (typeof window === "undefined") return 1280;
  return window.innerWidth;
}

/** Track whether the window is in the overlay-sidebar range. */
export function useSidebarNarrowViewport(): boolean {
  const [narrow, setNarrow] = useState(() =>
    sidebarUsesOverlay(readViewportWidth()),
  );

  useEffect(() => {
    // Test environments and older webviews have no matchMedia; they keep
    // the width read at mount.
    if (
      typeof window === "undefined" ||
      typeof window.matchMedia !== "function"
    ) {
      return;
    }
    const media = window.matchMedia(
      `(max-width: ${SIDEBAR_OVERLAY_MAX_WIDTH}px)`,
    );
    const apply = () => setNarrow(media.matches);
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, []);

  return narrow;
}

/**
 * How the rail should present: in the layout, as an overlay, or gone.
 *
 * Crossing the overlay breakpoint never writes the remembered width or the
 * collapsed preference. Opening the rail while narrow only toggles the
 * overlay.
 */
export function useSidebarLayout() {
  const collapsed = useUiStore((state) => state.sidebarCollapsed);
  const overlayOpen = useUiStore((state) => state.sidebarOverlayOpen);
  const toggleSidebar = useUiStore((state) => state.toggleSidebar);
  const setSidebarOverlayOpen = useUiStore(
    (state) => state.setSidebarOverlayOpen,
  );
  const narrow = useSidebarNarrowViewport();

  useEffect(() => {
    if (!narrow && overlayOpen) setSidebarOverlayOpen(false);
  }, [narrow, overlayOpen, setSidebarOverlayOpen]);

  const overlay = narrow && overlayOpen;
  const inLayout = narrow ? overlay : !collapsed;
  const showExpandStrip = narrow ? !overlayOpen : collapsed;

  function toggleVisibility() {
    if (narrow) {
      setSidebarOverlayOpen(!overlayOpen);
      return;
    }
    toggleSidebar();
  }

  return {
    narrow,
    overlay,
    inLayout,
    showExpandStrip,
    toggleVisibility,
  };
}
