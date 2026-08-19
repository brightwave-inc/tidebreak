import type { RefObject } from "react";
import { useEffect } from "react";

import { openExternal } from "@/host";

/**
 * The viewer chrome follows Tidebreak's live palette. Individual pages and
 * slides may still keep their authored paper/slide colors, but controls and
 * the surrounding canvas should never require a reload after a theme change.
 */
export const DOCUMENT_VIEWER_SURFACE = "bg-background text-foreground";

/**
 * Document engines may materialize hyperlinks after their initial render. Keep
 * bookmarks inside the document, remove unsafe external schemes, and route
 * HTTPS links through the host instead of navigating the desktop webview.
 */
export function useSecureViewerLinks(
  containerRef: RefObject<HTMLElement | null>,
): void {
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;

    const secureLinks = () => {
      for (const anchor of container.querySelectorAll<HTMLAnchorElement>("a")) {
        const href = anchor.getAttribute("href");
        if (!href || href.startsWith("#")) continue;

        const safeHref = safeExternalHref(href);
        if (!safeHref) {
          anchor.removeAttribute("href");
          anchor.removeAttribute("target");
          anchor.removeAttribute("rel");
          continue;
        }

        anchor.href = safeHref;
        anchor.target = "_blank";
        anchor.rel = "noopener noreferrer";
      }
    };

    secureLinks();
    const observer = new MutationObserver(secureLinks);
    observer.observe(container, {
      attributes: true,
      attributeFilter: ["href"],
      childList: true,
      subtree: true,
    });

    const handleClick = (event: MouseEvent) => {
      const target = event.target as Element | null;
      const anchor = target?.closest?.("a") as HTMLAnchorElement | null;
      if (!anchor || !container.contains(anchor)) return;

      const href = anchor.getAttribute("href");
      if (!href || href.startsWith("#")) return;

      event.preventDefault();
      event.stopPropagation();

      const safeHref = safeExternalHref(href);
      if (!safeHref) return;

      void openExternal(safeHref)
        .catch(() => false)
        .then((opened) => {
          if (!opened) {
            window.open(safeHref, "_blank", "noopener,noreferrer");
          }
        });
    };

    container.addEventListener("click", handleClick, true);
    return () => {
      observer.disconnect();
      container.removeEventListener("click", handleClick, true);
    };
  }, [containerRef]);
}

export function safeExternalHref(href: string): string | null {
  try {
    const parsed = new URL(href);
    return parsed.protocol === "https:" && !parsed.username && !parsed.password
      ? parsed.href
      : null;
  } catch {
    return null;
  }
}
