import { useNavigate, useRouterState } from "@tanstack/react-router";
import { ArrowLeft, PanelLeftOpen } from "lucide-react";

import { useManagedPolicy } from "@/managedPolicy";
import { settingsSectionsFor } from "@/settings/sections";
import { useUiStore } from "@/UiStore";
import { Sidebar, SidebarButton, SidebarContent, useSidebarWidth } from "./primitives";

/**
 * The rail while settings is open: the way back to the app, then a link to each
 * section. It replaces the app rail rather than sitting beside it — a second
 * column of navigation is one too many, and the way back is what the reader is
 * here to find.
 *
 * The active section is read from the URL, not from local state, so a link is
 * highlighted because it names where the reader is — the same reason the
 * section deep-links at all.
 */
export function SettingsSidebar({ onBack }: { onBack: () => void }) {
  const navigate = useNavigate();
  const toggleSidebar = useUiStore((state) => state.toggleSidebar);
  const isCompact = useSidebarWidth() === "compact";
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const { managed } = useManagedPolicy();
  const sections = settingsSectionsFor(managed);

  return (
    <Sidebar>
      <SidebarContent className="mt-2 gap-1 overflow-y-auto px-2">
        {isCompact && (
          <SidebarButton aria-label="Expand sidebar" onClick={toggleSidebar}>
            <PanelLeftOpen />
            <span>Expand sidebar</span>
          </SidebarButton>
        )}
        <SidebarButton className="text-muted-foreground" onClick={onBack}>
          <ArrowLeft />
          <span>Back to app</span>
        </SidebarButton>
        {sections.map((section) => {
          const to = `/settings/${section.path}`;
          const active = pathname === to;
          const Icon = section.icon;
          return (
            <SidebarButton
              key={section.path}
              aria-current={active ? "page" : undefined}
              data-active={active || undefined}
              className="data-[active]:bg-muted"
              onClick={() => void navigate({ to })}
            >
              <Icon />
              <span>{section.label}</span>
            </SidebarButton>
          );
        })}
      </SidebarContent>
    </Sidebar>
  );
}
