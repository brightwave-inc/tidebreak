import { ChevronLeft, ChevronRight } from "lucide-react";

import { WithTooltip } from "@/components/ui/tooltip";
import { useDesktopNavigation } from "./DesktopNavigation";

/**
 * The macOS overlay titlebar: the strip the traffic lights sit in, which the
 * app has to furnish itself because the system chrome is hidden.
 *
 * It carries navigation and the app's name and nothing else. The rail's own
 * collapse control stays in the rail — it acts on the rail, and putting a
 * second copy up here left two buttons for one job on adjacent rows.
 */
export function Titlebar() {
  const navigation = useDesktopNavigation();

  return (
    <div className="titlebar" data-tauri-drag-region>
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
        OpenWave
      </span>
    </div>
  );
}
