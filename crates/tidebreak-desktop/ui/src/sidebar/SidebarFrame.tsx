import { getName } from "@tauri-apps/api/app";
import { useEffect, useState, type ComponentType, type ReactNode } from "react";
import { useNavigate } from "@tanstack/react-router";
import { Monitor, Moon, PanelLeftClose, Settings, Sun } from "lucide-react";

import { useApp } from "@/AppContext";
import { WithTooltip } from "@/components/ui/tooltip";
import { hasNativeHost } from "@/host";
import { Logomark } from "@/Logomark";
import {
  Sidebar as SidebarRail,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
} from "./primitives";
import { useTheme } from "@/theme";
import { useUiStore } from "@/UiStore";

/**
 * The parts of the rail that do not depend on where the reader is: the way
 * home, the collapse control, and the app's own settings.
 *
 * The middle is supplied by whoever is rendering the rail — {@link AppSidebar}
 * everywhere but settings, which has its own section list and no use for a
 * chat list beside it.
 */
export function SidebarFrame({ children }: { children: ReactNode }) {
  const navigate = useNavigate();
  const { updateState } = useApp();
  const { mode: themeMode, cycle: cycleTheme } = useTheme();
  const toggleSidebar = useUiStore((state) => state.toggleSidebar);
  const [appName, setAppName] = useState("Tidebreak");

  // Keep dev and staging windows distinguishable, but put that identity in
  // the rail where it belongs instead of restoring a duplicate top header.
  useEffect(() => {
    if (!hasNativeHost()) return;
    let cancelled = false;
    getName().then(
      (name) => {
        if (!cancelled) setAppName(name);
      },
      () => {},
    );
    return () => {
      cancelled = true;
    };
  }, []);

  const updateReady = updateState.status === "ready";

  return (
    <SidebarRail>
      <SidebarHeader>
        <button
          type="button"
          className="inline-flex min-w-0 shrink cursor-pointer items-center gap-2.5 rounded-md p-1 text-foreground transition-opacity hover:opacity-70"
          aria-label="Home"
          onClick={() => void navigate({ to: "/" })}
        >
          {/* The mark's own ratio beside the name, the same lockup as the
              public site header. */}
          <Logomark width="30" height="16" />
          <span className="truncate font-mono text-sm font-medium leading-none">
            {appName}
          </span>
        </button>
        <span className="grow" />
        <WithTooltip label="Collapse sidebar" side="bottom">
          <button
            type="button"
            className="cursor-pointer rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
            aria-label="Collapse sidebar"
            onClick={toggleSidebar}
          >
            <PanelLeftClose size={15} />
          </button>
        </WithTooltip>
      </SidebarHeader>

      {/* min-h-0 so the chat list inside can be the part that scrolls: without
          it the content column grows to its content and the rail's own rows
          scroll away with the list. */}
      <SidebarContent className="min-h-0 gap-1 overflow-y-auto px-2">
        {children}
      </SidebarContent>

      <SidebarFooter className="border-t border-border-subtle px-2 py-1.5">
        <div className="flex items-center gap-1" aria-label="App controls">
          <SidebarUtilityButton
            label={updateReady ? "Settings, update ready" : "Settings"}
            icon={Settings}
            onClick={() => void navigate({ to: "/settings" })}
            indicator={updateReady}
          />
          <SidebarUtilityButton
            label={`Theme: ${themeMode}. Click to change.`}
            icon={
              themeMode === "light"
                ? Sun
                : themeMode === "dark"
                  ? Moon
                  : Monitor
            }
            onClick={cycleTheme}
          />
        </div>
      </SidebarFooter>
    </SidebarRail>
  );
}

function SidebarUtilityButton({
  label,
  icon: Icon,
  indicator = false,
  onClick,
}: {
  label: string;
  icon: ComponentType<{ size?: number }>;
  indicator?: boolean;
  onClick: () => void;
}) {
  return (
    <WithTooltip label={label} side="top">
      <button
        type="button"
        className="relative grid size-8 cursor-pointer place-items-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/25"
        aria-label={label}
        onClick={onClick}
      >
        <Icon size={15} />
        {indicator && (
          <span
            className="absolute top-1.5 right-1.5 size-1.5 rounded-full bg-success"
            aria-hidden="true"
          />
        )}
      </button>
    </WithTooltip>
  );
}
