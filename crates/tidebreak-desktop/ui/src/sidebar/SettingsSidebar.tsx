import { useNavigate, useRouterState } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";

import { useExperimentalFlags } from "@/experimental";
import { useManagedPolicy } from "@/managedPolicy";
import { settingsSectionsFor } from "@/settings/sections";
import { Sidebar, SidebarButton, SidebarContent } from "./primitives";

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
  const pathname = useRouterState({ select: (state) => state.location.pathname });
  const { managed } = useManagedPolicy();
  const codeModeEnabled = useExperimentalFlags((state) => state.codeModeEnabled);
  const sections = settingsSectionsFor(managed, codeModeEnabled);

  return (
    <Sidebar>
      <SidebarContent className="mt-2 gap-1 overflow-y-auto px-2">
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
              // Replace rather than push: the whole settings visit occupies one
              // history entry, so "Back to app" is a single step out no matter
              // how many sections were browsed.
              onClick={() => void navigate({ to, replace: true })}
            >
              <Icon className={active ? section.iconClass : undefined} />
              <span>{section.label}</span>
            </SidebarButton>
          );
        })}
      </SidebarContent>
    </Sidebar>
  );
}
