import type { ReactNode } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import {
  Monitor,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  RotateCw,
  Settings,
  Sun,
} from "lucide-react";

import { useApp } from "@/AppContext";
import { WithTooltip } from "@/components/ui/tooltip";
import { Logomark } from "@/Logomark";
import {
  Sidebar as SidebarRail,
  SidebarButton,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  useSidebarWidth,
} from "./primitives";
import { useUiStore } from "@/UiStore";

/**
 * The parts of the rail that do not depend on where the reader is: the way
 * home, the collapse control, and the app's own settings.
 *
 * Each route supplies the middle for itself, because that is the part that
 * differs — a rail shared across every route has to carry the union of what
 * every route needs, and the routes it does not fit are left holding controls
 * they cannot use.
 */
export function SidebarFrame({ children }: { children: ReactNode }) {
  const navigate = useNavigate();
  const { themeMode, cycleTheme, updateState, restartForUpdate } = useApp();
  const toggleSidebar = useUiStore((state) => state.toggleSidebar);
  const isCompact = useSidebarWidth() === "compact";
  const pathname = useRouterState({ select: (state) => state.location.pathname });

  const updateReady = updateState.status === "ready";

  return (
    <SidebarRail>
      <SidebarHeader>
        <button
          type="button"
          className="inline-flex shrink-0 cursor-pointer items-center rounded-md p-1 text-foreground transition-opacity hover:opacity-70"
          aria-label="Home"
          onClick={() => void navigate({ to: "/" })}
        >
          {/* Square box around the wide mark, so the home button lines up with
              the column of square rail buttons under it. */}
          <Logomark width="28" height="28" />
        </button>
        <span className="grow" />
        {!isCompact && (
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
        )}
      </SidebarHeader>

      <SidebarContent className="gap-1 overflow-y-auto px-2">
        {isCompact && (
          <SidebarButton aria-label="Expand sidebar" onClick={toggleSidebar}>
            <PanelLeftOpen />
            <span>Expand sidebar</span>
          </SidebarButton>
        )}
        {children}
      </SidebarContent>

      <SidebarFooter className="flex flex-col gap-0.5">
        {updateReady && (
          <SidebarButton onClick={restartForUpdate}>
            <RotateCw />
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
          {themeMode === "light" ? <Sun /> : themeMode === "dark" ? <Moon /> : <Monitor />}
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
