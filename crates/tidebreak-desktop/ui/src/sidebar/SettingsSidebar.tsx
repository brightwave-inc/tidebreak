import { useEffect, useId, useRef } from "react";
import { useNavigate, useRouterState } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";

import { useManagedPolicy } from "@/managedPolicy";
import { settingsSectionGroupsFor } from "@/settings/sections";
import {
  Sidebar,
  SidebarButton,
  SidebarContent,
  SidebarHeader,
  SidebarSectionTitle,
} from "./primitives";

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
  const pathname = useRouterState({
    select: (state) => state.location.pathname,
  });
  const { managed } = useManagedPolicy();
  const groups = settingsSectionGroupsFor(managed);
  const groupIdPrefix = useId();
  const navigationRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    navigationRef.current
      ?.querySelector<HTMLElement>('[aria-current="page"]')
      ?.scrollIntoView({ block: "nearest" });
  }, [pathname]);

  return (
    <Sidebar className="settings-sidebar">
      <SidebarHeader className="settings-sidebar-header">
        <SidebarButton
          className="settings-back-button"
          aria-label="Back to app"
          onClick={onBack}
        >
          <ArrowLeft />
          <span className="settings-back-copy">
            <span className="settings-back-title">Settings</span>
            <span className="settings-back-label">Back to app</span>
          </span>
        </SidebarButton>
      </SidebarHeader>
      <SidebarContent
        asChild
        className="settings-sidebar-content mt-0 overflow-y-auto px-2 py-2"
      >
        <nav ref={navigationRef} aria-label="Settings sections">
          {groups.map((group) => {
            const titleId = `${groupIdPrefix}-${group.id}`;
            return (
              <div className="settings-sidebar-group" key={group.id}>
                <SidebarSectionTitle
                  id={titleId}
                  className="settings-sidebar-group-title"
                >
                  {group.label}
                </SidebarSectionTitle>
                <div
                  className="settings-sidebar-group-items"
                  role="group"
                  aria-labelledby={titleId}
                >
                  {group.sections.map((section) => {
                    const to = `/settings/${section.path}`;
                    const active = pathname === to;
                    const Icon = section.icon;
                    return (
                      <SidebarButton
                        key={section.path}
                        aria-current={active ? "page" : undefined}
                        data-active={active || undefined}
                        className="settings-sidebar-item"
                        // Replace rather than push: the whole settings visit occupies
                        // one history entry, so Back is a single step out no matter
                        // how many sections were browsed.
                        onClick={() => void navigate({ to, replace: true })}
                      >
                        <Icon
                          className={active ? section.iconClass : undefined}
                        />
                        <span>{section.label}</span>
                      </SidebarButton>
                    );
                  })}
                </div>
              </div>
            );
          })}
        </nav>
      </SidebarContent>
    </Sidebar>
  );
}
