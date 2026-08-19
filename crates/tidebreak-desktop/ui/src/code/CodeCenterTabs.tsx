import { useRef, type KeyboardEvent, type ReactNode } from "react";
import {
  ArrowRightFromLine,
  CircleX,
  Copy,
  FileCode,
  FileDiff,
  ListX,
  MessageSquare,
  X,
} from "lucide-react";

import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuLabel,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { cn } from "@/lib/utils";
import { panelKey, type PanelContent } from "@/panel/panelTypes";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";

/** Ids the strip and the two center panels agree on, so tabs name panels. */
export const CHAT_TAB_ID = "code-center-tab-chat";
export const CHAT_PANEL_ID = "code-center-panel-chat";
export const EDITOR_PANEL_ID = "code-center-panel-editor";
const editorTabId = (index: number) => `code-center-tab-editor-${index}`;

/** Which tab labels the editor panel right now. */
export function centerEditorTabId(index: number): string {
  return editorTabId(index);
}

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
  onCloseAllEditors,
  onCloseOtherEditors,
  onCloseEditorsToRight,
  onCopyPath,
}: {
  editorTabs: PanelContent[];
  editorActiveIndex: number;
  conversationFocused: boolean;
  onSelectChat: () => void;
  onSelectEditor: (index: number) => void;
  onCloseEditor: (index: number) => void;
  onCloseAllEditors: () => void;
  onCloseOtherEditors: (index: number) => void;
  onCloseEditorsToRight: (index: number) => void;
  onCopyPath: (path: string) => void;
}) {
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([]);

  if (editorTabs.length === 0) return null;

  /**
   * The tabs pattern: one tab stop for the strip, arrows to move between
   * tabs. Selection follows focus here because both are cheap — moving to a
   * tab is what opening it means.
   */
  function select(position: number) {
    const last = editorTabs.length;
    const wrapped = position < 0 ? last : position > last ? 0 : position;
    if (wrapped === 0) onSelectChat();
    else onSelectEditor(wrapped - 1);
    tabRefs.current[wrapped]?.focus();
  }

  function onKeyDown(event: KeyboardEvent<HTMLButtonElement>, position: number) {
    if (event.key === "ArrowRight") {
      event.preventDefault();
      select(position + 1);
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      select(position - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      select(0);
    } else if (event.key === "End") {
      event.preventDefault();
      select(editorTabs.length);
    }
  }

  return (
    <div
      className="flex shrink-0 items-center gap-1 overflow-x-auto border-b px-2 py-1"
      role="tablist"
      aria-label="Workspace center"
    >
      <ContextMenu>
        <ContextMenuTrigger asChild>
          <button
            type="button"
            role="tab"
            id={CHAT_TAB_ID}
            aria-selected={conversationFocused}
            aria-controls={CHAT_PANEL_ID}
            tabIndex={conversationFocused ? 0 : -1}
            ref={(node) => {
              tabRefs.current[0] = node;
            }}
            className={cn(
              "flex shrink-0 cursor-pointer items-center gap-1.5 rounded-md px-2 py-1 text-xs font-medium",
              FOCUS_RING_TIGHT,
              HOVER_TINT,
              conversationFocused
                ? "bg-muted text-foreground"
                : "text-muted-foreground hover:bg-muted/60 hover:text-foreground",
            )}
            onClick={onSelectChat}
            onKeyDown={(event) => onKeyDown(event, 0)}
          >
            <MessageSquare className="size-3.5" />
            Chat
          </button>
        </ContextMenuTrigger>
        <TabContextMenuContent label="Chat">
          <ContextMenuItem
            className="gap-3 py-2"
            disabled={editorTabs.length === 0}
            onSelect={onCloseAllEditors}
          >
            <ListX />
            Close other tabs
          </ContextMenuItem>
        </TabContextMenuContent>
      </ContextMenu>
      {editorTabs.map((panel, index) => {
        const active = !conversationFocused && index === editorActiveIndex;
        const { name, suffix } = centerTabParts(panel);
        const label = suffix ? `${name} ${suffix}` : name;
        const path =
          panel.type === "file" || panel.type === "diff"
            ? panel.path
            : undefined;
        return (
          <ContextMenu key={panelKey(panel)}>
            <ContextMenuTrigger asChild>
              {/* A tablist owns tabs. The close control is a second control on
                  the same row, so this pair is transparent to assistive tech. */}
              <div
                role="presentation"
                className={cn(
                  "flex min-w-0 shrink-0 items-center rounded-md pr-1",
                  HOVER_TINT,
                  active ? "bg-muted" : "hover:bg-muted/60",
                )}
              >
                <button
                  type="button"
                  role="tab"
                  id={editorTabId(index)}
                  aria-label={label}
                  aria-selected={active}
                  aria-controls={EDITOR_PANEL_ID}
                  tabIndex={active ? 0 : -1}
                  ref={(node) => {
                    tabRefs.current[index + 1] = node;
                  }}
                  className={cn(
                    "flex min-w-0 cursor-pointer items-center gap-1.5 rounded-md py-1 pr-1 pl-2 text-xs font-medium",
                    FOCUS_RING_TIGHT,
                    HOVER_TINT,
                    active ? "text-foreground" : "text-muted-foreground",
                  )}
                  onClick={() => onSelectEditor(index)}
                  onKeyDown={(event) => onKeyDown(event, index + 1)}
                >
                  {panel.type === "diff" ? (
                    <FileDiff className="size-3.5 shrink-0" />
                  ) : (
                    <FileCode className="size-3.5 shrink-0" />
                  )}
                  <span
                    className="flex min-w-0 items-baseline gap-1"
                    title={centerTabTitle(panel)}
                  >
                    <span className="max-w-40 truncate">{name}</span>
                    {/* The suffix stays outside the truncating name. */}
                    {suffix && <span className="shrink-0">{suffix}</span>}
                  </span>
                </button>
                <button
                  type="button"
                  className={cn(
                    "text-muted-foreground hover:text-foreground grid size-4 shrink-0 cursor-pointer place-items-center rounded-sm",
                    FOCUS_RING_TIGHT,
                    HOVER_TINT,
                  )}
                  onClick={() => onCloseEditor(index)}
                >
                  <X className="size-3" />
                  <span className="sr-only">{`Close ${label}`}</span>
                </button>
              </div>
            </ContextMenuTrigger>
            <TabContextMenuContent label={label}>
              {path && (
                <>
                  <ContextMenuItem
                    className="gap-3 py-2"
                    onSelect={() => onCopyPath(path)}
                  >
                    <Copy />
                    Copy path
                  </ContextMenuItem>
                  <ContextMenuSeparator />
                </>
              )}
              <ContextMenuItem
                className="gap-3 py-2"
                onSelect={() => onCloseEditor(index)}
              >
                <X />
                Close tab
              </ContextMenuItem>
              <ContextMenuItem
                className="gap-3 py-2"
                disabled={editorTabs.length <= 1}
                onSelect={() => onCloseOtherEditors(index)}
              >
                <ListX />
                Close other tabs
              </ContextMenuItem>
              <ContextMenuItem
                className="gap-3 py-2"
                disabled={index === editorTabs.length - 1}
                onSelect={() => onCloseEditorsToRight(index)}
              >
                <ArrowRightFromLine />
                Close tabs to the right
              </ContextMenuItem>
              <ContextMenuItem
                className="gap-3 py-2"
                onSelect={onCloseAllEditors}
              >
                <CircleX />
                Close all tabs
              </ContextMenuItem>
            </TabContextMenuContent>
          </ContextMenu>
        );
      })}
    </div>
  );
}

function TabContextMenuContent({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  return (
    <ContextMenuContent className="min-w-56 rounded-xl border-border/70 bg-popover/95 p-1.5 shadow-2xl backdrop-blur-xl">
      <ContextMenuLabel className="max-w-52 truncate">{label}</ContextMenuLabel>
      {children}
    </ContextMenuContent>
  );
}

/** The whole path behind a tab whose label is only the file name. */
function centerTabTitle(panel: PanelContent): string {
  if (panel.type === "file") return panel.path;
  if (panel.type === "diff" && panel.path) return `${panel.path} (diff)`;
  const { name, suffix } = centerTabParts(panel);
  return suffix ? `${name} ${suffix}` : name;
}

/**
 * A tab's label, split into the part that may truncate and the part that may
 * not.
 */
function centerTabParts(panel: PanelContent): {
  name: string;
  suffix: string | null;
} {
  if (panel.type === "file") {
    return { name: panel.path.split("/").pop() || panel.path, suffix: null };
  }
  if (panel.type === "diff") {
    if (panel.path) {
      return {
        name: panel.path.split("/").pop() || panel.path,
        suffix: "(diff)",
      };
    }
    return { name: panel.turnId ? "Turn diff" : "Diff", suffix: null };
  }
  return { name: panel.type, suffix: null };
}
