import type { ReactNode, ComponentProps } from "react";
import { ChevronsDownUp, ChevronsUpDown, FolderPlus, Plus } from "lucide-react";
import { cn } from "@/lib/utils";
import { RAIL_ICON_BUTTON } from "./interactive";
import { RailSettingsMenu } from "./RailSettingsMenu";

/** Keep workspace creation controls beside grouping and display settings. */
export function WorkspaceRailToolbar({
  children,
  collapseKeys,
  collapsedKeys,
  onCollapsedChange,
  onAddRepo,
  onNewWorkspace,
  settings,
  className,
}: {
  children: ReactNode;
  collapseKeys: readonly string[];
  collapsedKeys: readonly string[];
  onCollapsedChange: (keys: readonly string[]) => void;
  onAddRepo: () => void;
  onNewWorkspace: () => void;
  settings?: ComponentProps<typeof RailSettingsMenu>;
  className?: string;
}) {
  const allCollapsed =
    collapseKeys.length > 0 &&
    collapseKeys.every((key) => collapsedKeys.includes(key));
  const collapseLabel = allCollapsed
    ? "Expand all groups"
    : "Collapse all groups";
  return (
    <div
      role="toolbar"
      aria-label="Workspace actions"
      className={cn(
        "flex shrink-0 items-center gap-0.5 px-1 pt-1 pb-1.5",
        className,
      )}
    >
      {children}
      <button
        type="button"
        className={RAIL_ICON_BUTTON}
        aria-label={collapseLabel}
        title={collapseLabel}
        disabled={collapseKeys.length === 0}
        onClick={() =>
          onCollapsedChange(
            allCollapsed
              ? collapsedKeys.filter((key) => !collapseKeys.includes(key))
              : [...new Set([...collapsedKeys, ...collapseKeys])],
          )
        }
      >
        {allCollapsed ? (
          <ChevronsUpDown size={15} />
        ) : (
          <ChevronsDownUp size={15} />
        )}
      </button>
      <RailSettingsMenu {...settings} />
      <button
        type="button"
        className={RAIL_ICON_BUTTON}
        aria-label="Add repo"
        title="Add repo"
        onClick={onAddRepo}
      >
        <FolderPlus size={15} />
      </button>
      <button
        type="button"
        className={RAIL_ICON_BUTTON}
        aria-label="New workspace"
        title="New workspace"
        onClick={onNewWorkspace}
      >
        <Plus size={15} />
      </button>
    </div>
  );
}
