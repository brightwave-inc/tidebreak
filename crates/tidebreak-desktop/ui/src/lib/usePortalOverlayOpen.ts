import { useEffect, useState } from "react";

/**
 * True while any app-owned portaled dialog, menu, or listbox is open.
 *
 * Radix-based overlays are portaled as direct body children and use
 * `data-state` attributes to signal visibility.  The hook observes both of
 * those DOM signals (attribute changes + child-list mutations) so it
 * reacts to opens and closes without missing fast transitions.
 *
 * Callers use this to hide a native child webview while DOM overlays
 * are on screen, since the native surface always paints above web content.
 */
export function usePortalOverlayOpen(): boolean {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    const selector = [
      '[role="dialog"][data-state="open"]',
      '[role="alertdialog"][data-state="open"]',
      '[role="menu"][data-state="open"]',
      '[role="listbox"][data-state="open"]',
    ].join(",");
    let frame: number | null = null;
    const update = () => {
      if (frame !== null) return;
      frame = window.requestAnimationFrame(() => {
        frame = null;
        setOpen(document.querySelector(selector) !== null);
      });
    };
    const stateObserver = new MutationObserver(update);
    stateObserver.observe(document.body, {
      attributes: true,
      attributeFilter: ["data-state", "role"],
      subtree: true,
    });
    // Radix portals are direct body children.  Keep transcript and editor DOM
    // churn out of this observer so streaming content does not trigger global
    // overlay queries.
    const portalObserver = new MutationObserver(update);
    portalObserver.observe(document.body, { childList: true });
    update();
    return () => {
      stateObserver.disconnect();
      portalObserver.disconnect();
      if (frame !== null) window.cancelAnimationFrame(frame);
    };
  }, []);

  return open;
}
