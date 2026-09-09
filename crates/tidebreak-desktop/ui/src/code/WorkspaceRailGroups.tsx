import { useId, type ReactNode } from "react";
import { ChevronDown } from "lucide-react";
import type { CodeWorkspaceSnapshot } from "@/api/types";
import { cn } from "@/lib/utils";
import { FOCUS_RING_INSET } from "./interactive";
import { WorkspaceStatusMark } from "./WorkspaceCard";
import {
  isWorkspaceStatusRank,
  workspaceGroupCollapseKey,
  workspaceSourceCollapseKey,
  type WorkspaceSortMode,
  type WorkspaceSourceSection,
} from "./workspaceCards";

function Disclosure({
  label,
  count,
  expanded,
  onToggle,
  level,
  children,
  mark,
  sectionKey,
}: {
  label: string;
  count: number;
  expanded: boolean;
  onToggle: () => void;
  level: "source" | "group";
  children: ReactNode;
  mark?: ReactNode;
  sectionKey: string;
}) {
  const id = useId();
  return (
    <section
      data-rail-section={sectionKey}
      aria-label={label}
      className={cn(
        "min-w-0",
        level === "source" ? "mb-3 last:mb-0" : "mb-1 last:mb-0",
      )}
    >
      <h3 className="min-w-0">
        <button
          type="button"
          aria-label={`${label}, ${count} ${count === 1 ? "workspace" : "workspaces"}`}
          aria-expanded={expanded}
          aria-controls={id}
          onClick={onToggle}
          className={cn(
            "flex w-full min-w-0 cursor-pointer items-center gap-1.5 rounded-md px-2 text-left hover:bg-muted/70",
            FOCUS_RING_INSET,
            level === "source"
              ? "h-8 text-sm font-semibold text-foreground"
              : "h-7 text-xs font-medium text-muted-foreground hover:text-foreground",
          )}
        >
          <ChevronDown
            aria-hidden
            className={cn(
              "size-3 shrink-0 text-muted-foreground motion-safe:transition-transform",
              !expanded && "-rotate-90",
            )}
          />
          {mark && (
            <span aria-hidden className="shrink-0">
              {mark}
            </span>
          )}
          <span className="min-w-0 flex-1 truncate" title={label}>
            {label}
          </span>
          <span className="shrink-0 font-mono text-2xs font-normal tabular-nums text-muted-foreground">
            {count}
          </span>
        </button>
      </h3>
      <div
        id={id}
        hidden={!expanded}
        className={level === "source" ? "pl-2" : "pl-3"}
      >
        {children}
      </div>
    </section>
  );
}

/** Source headings appear only when the list contains more than one source. */
export function WorkspaceRailGroups({
  sections,
  mode,
  collapsedKeys,
  onToggle,
  renderWorkspace,
}: {
  sections: readonly WorkspaceSourceSection[];
  mode: WorkspaceSortMode;
  collapsedKeys: readonly string[];
  onToggle: (key: string) => void;
  renderWorkspace: (workspace: CodeWorkspaceSnapshot) => ReactNode;
}) {
  const multipleSources = sections.length > 1;
  function renderGroups(section: WorkspaceSourceSection) {
    return section.groups.map((group) => {
      const key = workspaceGroupCollapseKey(section.key, mode, group.key);
      const rows = group.workspaces.map(renderWorkspace);
      return group.label ? (
        <Disclosure
          key={key}
          sectionKey={key}
          label={group.label}
          count={group.workspaces.length}
          expanded={!collapsedKeys.includes(key)}
          onToggle={() => onToggle(key)}
          level="group"
          mark={
            isWorkspaceStatusRank(group.key) ? (
              <WorkspaceStatusMark rank={group.key} />
            ) : undefined
          }
        >
          {rows}
        </Disclosure>
      ) : (
        <div key={key} className="min-w-0">
          {rows}
        </div>
      );
    });
  }
  return sections.map((section) => {
    const sourceKey = workspaceSourceCollapseKey(section.key);
    return multipleSources ? (
      <Disclosure
        key={section.key}
        sectionKey={sourceKey}
        label={section.label}
        count={section.groups.reduce(
          (total, group) => total + group.workspaces.length,
          0,
        )}
        expanded={!collapsedKeys.includes(sourceKey)}
        onToggle={() => onToggle(sourceKey)}
        level="source"
      >
        {renderGroups(section)}
      </Disclosure>
    ) : (
      <div key={section.key} data-rail-source={section.key}>
        {renderGroups(section)}
      </div>
    );
  });
}
