import { getName } from "@tauri-apps/api/app";
import { useEffect, useState, type ReactNode } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import {
  Monitor,
  Moon,
  PanelLeftClose,
  RotateCw,
  Settings,
  Sun,
} from "lucide-react";

import { useApp } from "@/AppContext";
import { WithTooltip } from "@/components/ui/tooltip";
import { hasNativeHost } from "@/host";
import { Logomark } from "@/Logomark";
import {
  Sidebar as SidebarRail,
  SidebarButton,
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
export function SidebarFrame({
  children,
  footer,
}: {
  children: ReactNode;
  footer?: ReactNode;
}) {
  const navigate = useNavigate();
  const { updateState, restartForUpdate } = useApp();
  const { mode: themeMode, cycle: cycleTheme } = useTheme();
  const toggleSidebar = useUiStore((state) => state.toggleSidebar);
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
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

      <SidebarFooter className="flex flex-col gap-0.5">
        {footer}
        {updateReady && (
          <SidebarButton onClick={restartForUpdate}>
            <RotateCw className="text-success" />
            <span>Restart to update</span>
            {updateState.version && (
              <span className="ml-auto text-xs text-muted-foreground">
                v{updateState.version}
              </span>
            )}
          </SidebarButton>
        )}
        <SidebarButton
          aria-label={`Theme: ${themeMode}. Click to change.`}
          onClick={cycleTheme}
        >
          {themeMode === "light" ? (
            <Sun />
          ) : themeMode === "dark" ? (
            <Moon />
          ) : (
            <Monitor />
          )}
          <span>Theme</span>
        </SidebarButton>
        <SidebarButton
          aria-current={pathname === "/settings" ? "page" : undefined}
          data-active={pathname === "/settings" || undefined}
          className="data-[active]:bg-muted"
          onClick={() => void navigate({ to: "/settings" })}
        >
          <Settings />
          <span>Settings</span>
        </SidebarButton>
      </SidebarFooter>
    </SidebarRail>
  );
}
