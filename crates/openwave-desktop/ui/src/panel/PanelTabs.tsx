import { Bot, FileText, FolderOpen, LayoutGrid, Package, Shapes, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { panelKey, type PanelContent } from "./panelTypes";

/**
 * What a tab says it is.
 *
 * Nothing here fetches: the strip has to be right the moment a panel opens,
 * before the panel behind it has loaded anything, so a tab names the kind of
 * thing it holds rather than the title of the particular one. The panel's own
 * header carries the title once it has it.
 */
function panelTabLabel(panel: PanelContent): string {
  switch (panel.type) {
    case "document":
      return "Document";
    case "outputs":
      return panel.outputId ? "Output" : "Outputs";
    case "folders":
      return "Folders";
    case "apps":
      return "Apps";
    case "plugins":
      return "Plugins";
    case "agent":
      return "Agent";
  }
}

function PanelTabIcon({ panel }: { panel: PanelContent }) {
  const className = "size-3.5 shrink-0";
  switch (panel.type) {
    case "document":
      return <FileText className={className} />;
    case "outputs":
      return <Package className={className} />;
    case "folders":
      return <FolderOpen className={className} />;
    case "apps":
      return <LayoutGrid className={className} />;
    case "plugins":
      return <Shapes className={className} />;
    case "agent":
      return <Bot className={className} />;
  }
}

/**
 * The strip above the region: one tab per open panel, the active one lifted
 * out of the muted trough the way the in-panel view switchers are.
 *
 * The close control is a sibling of the tab button rather than nested inside
 * it — a button inside a button is not valid markup, and clicking the ✕ should
 * not first select the tab it is about to remove.
 */
export function PanelTabs({
  tabs,
  activeIndex,
  onSelect,
  onClose,
}: {
  tabs: PanelContent[];
  activeIndex: number;
  onSelect: (index: number) => void;
  onClose: (index: number) => void;
}) {
  if (tabs.length === 0) return null;

  return (
    <div
      role="tablist"
      aria-label="Open panels"
      className="flex shrink-0 items-center gap-1 overflow-x-auto rounded-lg bg-muted p-1"
    >
      {tabs.map((panel, index) => {
        const active = index === activeIndex;
        const label = panelTabLabel(panel);
        return (
          <div
            key={panelKey(panel)}
            className={cn(
              "group flex min-w-0 shrink-0 items-center rounded-md pr-1 transition-colors",
              active ? "bg-background shadow-sm" : "hover:bg-background/50",
            )}
          >
            <button
              type="button"
              role="tab"
              aria-selected={active}
              onClick={() => onSelect(index)}
              className={cn(
                "flex min-w-0 cursor-pointer items-center gap-1.5 rounded-md py-1 pl-2 pr-1 text-xs font-medium whitespace-nowrap transition-colors",
                active ? "text-foreground" : "text-muted-foreground hover:text-foreground",
              )}
            >
              <PanelTabIcon panel={panel} />
              <span className="max-w-40 truncate">{label}</span>
            </button>
            <button
              type="button"
              onClick={() => onClose(index)}
              className="grid size-4 shrink-0 cursor-pointer place-items-center rounded-sm text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
            >
              <X className="size-3" />
              <span className="sr-only">{`Close ${label}`}</span>
            </button>
          </div>
        );
      })}
    </div>
  );
}
