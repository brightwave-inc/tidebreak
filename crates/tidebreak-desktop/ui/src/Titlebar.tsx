import { ChevronLeft, ChevronRight } from "lucide-react";

import { WithTooltip } from "@/components/ui/tooltip";
import type { DesktopNavigation } from "./DesktopNavigation";
import { cn } from "@/lib/utils";

/**
 * The desktop titlebar: navigation stays available even where the native
 * window decorations do not provide browser-style history controls.
 *
 * It carries only native navigation. Product identity lives in the sidebar,
 * so the window chrome can recede into the shell instead of reading as a
 * second application header above it.
 */
export function Titlebar({
  macOverlay,
  navigation,
}: {
  macOverlay: boolean;
  navigation: DesktopNavigation;
}) {
  return (
    <div
      className={cn("titlebar", macOverlay && "is-mac-overlay")}
      data-tauri-drag-region
    >
      <div className="titlebar-nav">
        <WithTooltip label="Back" side="bottom">
          <button
            type="button"
            className="inline-flex items-center justify-center size-6 rounded-md text-muted-foreground hover:not-disabled:text-foreground hover:not-disabled:bg-[color-mix(in_srgb,var(--ink)_8%,transparent)] disabled:opacity-40 disabled:cursor-default"
            aria-label="Back"
            disabled={!navigation.canGoBack}
            onClick={navigation.goBack}
          >
            <ChevronLeft size={16} />
          </button>
        </WithTooltip>
        <WithTooltip label="Forward" side="bottom">
          <button
            type="button"
            className="inline-flex items-center justify-center size-6 rounded-md text-muted-foreground hover:not-disabled:text-foreground hover:not-disabled:bg-[color-mix(in_srgb,var(--ink)_8%,transparent)] disabled:opacity-40 disabled:cursor-default"
            aria-label="Forward"
            disabled={!navigation.canGoForward}
            onClick={navigation.goForward}
          >
            <ChevronRight size={16} />
          </button>
        </WithTooltip>
      </div>
    </div>
  );
}
