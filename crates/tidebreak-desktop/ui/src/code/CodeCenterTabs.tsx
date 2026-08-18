import { FileCode, FileDiff, MessageSquare, X } from "lucide-react";

import { cn } from "@/lib/utils";
import { panelKey, type PanelContent } from "@/panel/panelTypes";

/**
 * Center strip: Chat is always first and cannot close. File and diff tabs
 * sit beside it.
 */
export function CodeCenterTabs({
  editorTabs,
  editorActiveIndex,
  conversationFocused,
  onSelectChat,
  onSelectEditor,
  onCloseEditor,
}: {
  editorTabs: PanelContent[];
  editorActiveIndex: number;
  conversationFocused: boolean;
  onSelectChat: () => void;
  onSelectEditor: (index: number) => void;
  onCloseEditor: (index: number) => void;
}) {
  if (editorTabs.length === 0) return null;

  return (
    <div
      className="flex shrink-0 items-center gap-1 overflow-x-auto border-b px-2 py-1"
      role="tablist"
      aria-label="Workspace center"
    >
      <button
        type="button"
        role="tab"
        aria-selected={conversationFocused}
        className={cn(
          "flex shrink-0 cursor-pointer items-center gap-1.5 rounded-md px-2 py-1 text-xs font-medium",
          conversationFocused
            ? "bg-muted text-foreground"
            : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
        )}
        onClick={onSelectChat}
      >
        <MessageSquare className="size-3.5" />
        Chat
      </button>
      {editorTabs.map((panel, index) => {
        const active = !conversationFocused && index === editorActiveIndex;
        const label = centerTabLabel(panel);
        return (
          <div
            key={panelKey(panel)}
            className={cn(
              "flex min-w-0 shrink-0 items-center rounded-md pr-1",
              active ? "bg-muted" : "hover:bg-muted/60",
            )}
          >
            <button
              type="button"
              role="tab"
              aria-selected={active}
              className={cn(
                "flex min-w-0 cursor-pointer items-center gap-1.5 py-1 pl-2 pr-1 text-xs font-medium",
                active ? "text-foreground" : "text-muted-foreground",
              )}
              onClick={() => onSelectEditor(index)}
            >
              {panel.type === "diff" ? (
                <FileDiff className="size-3.5 shrink-0" />
              ) : (
                <FileCode className="size-3.5 shrink-0" />
              )}
              <span className="max-w-40 truncate">{label}</span>
            </button>
            <button
              type="button"
              className="grid size-4 shrink-0 cursor-pointer place-items-center rounded-sm text-muted-foreground hover:text-foreground"
              onClick={() => onCloseEditor(index)}
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

function centerTabLabel(panel: PanelContent): string {
  if (panel.type === "file") {
    return panel.path.split("/").pop() || panel.path;
  }
  if (panel.type === "diff") {
    if (panel.path) return `${panel.path.split("/").pop() || panel.path} (diff)`;
    return panel.turnId ? "Turn diff" : "Diff";
  }
  return panel.type;
}
