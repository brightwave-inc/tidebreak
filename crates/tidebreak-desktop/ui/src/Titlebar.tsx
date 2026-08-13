import { getName } from "@tauri-apps/api/app";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { useEffect, useState } from "react";

import { WithTooltip } from "@/components/ui/tooltip";
import type { DesktopNavigation } from "./DesktopNavigation";
import { cn } from "@/lib/utils";

/**
 * The desktop titlebar: navigation stays available even where the native
 * window decorations do not provide browser-style history controls.
 *
 * It carries navigation and the app's name and nothing else. The rail's own
 * collapse control stays in the rail — it acts on the rail, and putting a
 * second copy up here left two buttons for one job on adjacent rows.
 */
export function Titlebar({
  macOverlay,
  navigation,
}: {
  macOverlay: boolean;
  navigation: DesktopNavigation;
}) {
  // The host owns the display name: debug reports "Tidebreak [dev]" and
  // staging reports "Tidebreak [staging]" so those windows are
  // distinguishable from an installed release. The titlebar only renders
  // inside the native host, where `getName` is always available.
  const [appName, setAppName] = useState("Tidebreak");
  useEffect(() => {
    let cancelled = false;
    getName().then((name) => {
      if (!cancelled) setAppName(name);
    }, () => {});
    return () => {
      cancelled = true;
    };
  }, []);

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
      <span className="titlebar-title" data-tauri-drag-region>
        {appName}
      </span>
    </div>
  );
}
