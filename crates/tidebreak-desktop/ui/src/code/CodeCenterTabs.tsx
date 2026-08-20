import { useRef, type KeyboardEvent, type ReactNode } from "react";
import { useDroppable } from "@dnd-kit/core";
import {
  SortableContext,
  horizontalListSortingStrategy,
  useSortable,
} from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import {
  ArrowRightFromLine,
  ChevronLeft,
  ChevronRight,
  CircleX,
  Columns2,
  Copy,
  FileCode,
  FileDiff,
  GitBranch,
  GitPullRequest,
  Globe2,
  SquareTerminal,
  ListX,
  MessageSquare,
  MoveLeft,
  MoveRight,
  PanelRightClose,
  Plus,
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
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { panelKey, type PanelContent } from "@/panel/panelTypes";
import { editorStripDropId, editorTabDragId } from "./editorDrag";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";

/** Ids the strip and the two center panels agree on, so tabs name panels. */
export const CHAT_TAB_ID = "code-center-tab-chat";
export const CHAT_PANEL_ID = "code-center-panel-chat";
export const EDITOR_PANEL_ID = "code-center-panel-editor-primary";
export const SPLIT_EDITOR_PANEL_ID = "code-center-panel-editor-secondary";
const editorTabId = (region: CenterTabRegion, index: number) =>
  `code-center-tab-editor-${region}-${index}`;

export type CenterTabRegion = "primary" | "secondary";

/** Which tab labels the editor panel right now. */
export function centerEditorTabId(
  index: number,
  region: CenterTabRegion = "primary",
): string {
  return editorTabId(region, index);
}

/**
 * Center strip: the main agent is always first and cannot close. File and diff
 * tabs sit beside it, and the plus control opens a new file tab.
 */
export function CodeCenterTabs({
  editorTabs,
  editorActiveIndex,
  conversationFocused,
  onSelectChat,
  onSelectEditor,
  onCloseEditor,
  onCloseAllEditors,
  onCloseEveryEditor,
  onCloseOtherEditors,
  onCloseEditorsToRight,
  onCopyPath,
  onNewTab,
  onNewBrowser,
  onNewDiff,
  onNewSourceControl,
  onNewPr,
  onNewTerminal,
  browserTitles = {},
  region = "primary",
  showMainAgent = true,
  onMoveEditorToOtherGroup,
  onMoveEditor,
  onSplitActive,
  onCloseGroup,
}: {
  editorTabs: PanelContent[];
  editorActiveIndex: number;
  conversationFocused: boolean;
  onSelectChat: () => void;
  onSelectEditor: (index: number) => void;
  onCloseEditor: (index: number) => void;
  onCloseAllEditors: () => void;
  /** Global close used by the persistent Main agent tab. */
  onCloseEveryEditor?: () => void;
  onCloseOtherEditors: (index: number) => void;
  onCloseEditorsToRight: (index: number) => void;
  onCopyPath: (path: string) => void;
  /** Open the file picker; the menu's "Open file" entry. */
  onNewTab: () => void;
  onNewBrowser?: () => void;
  /** Open the all-changes diff as a center tab. */
  onNewDiff?: () => void;
  /** Open source control as a center tab. */
  onNewSourceControl?: () => void;
  /** Open the pull request's details as a center tab. */
  onNewPr?: () => void;
  /** Open the workspace terminal. */
  onNewTerminal?: () => void;
  browserTitles?: Readonly<Record<string, string>>;
  region?: CenterTabRegion;
  showMainAgent?: boolean;
  onMoveEditorToOtherGroup?: (index: number) => void;
  /**
   * Reorder within this strip. Dragging does the same thing, so this is the
   * path for anyone not using a pointer.
   */
  onMoveEditor?: (from: number, to: number) => void;
  onSplitActive?: () => void;
  onCloseGroup?: () => void;
}) {
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const { setNodeRef: setStripRef } = useDroppable({
    id: editorStripDropId(region),
  });
  const tabOffset = showMainAgent ? 1 : 0;
  const panelId =
    region === "primary" ? EDITOR_PANEL_ID : SPLIT_EDITOR_PANEL_ID;

  /**
   * The tabs pattern: one tab stop for the strip, arrows to move between
   * tabs. Selection follows focus here because both are cheap — moving to a
   * tab is what opening it means.
   */
  function select(position: number) {
    const last = editorTabs.length + tabOffset - 1;
    const wrapped = position < 0 ? last : position > last ? 0 : position;
    if (showMainAgent && wrapped === 0) onSelectChat();
    else onSelectEditor(wrapped - tabOffset);
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
      select(editorTabs.length + tabOffset - 1);
    }
  }

  return (
    <div
      className="workspace-pane-tabs flex h-11 shrink-0 items-center gap-1 overflow-x-auto border-b border-border-subtle bg-page-background/55 px-2"
      role="tablist"
      // The strip takes drops on its own account, so releasing past the last
      // tab still lands in this group instead of falling through to nothing.
      ref={setStripRef}
      aria-label={region === "primary" ? "Workspace center" : "Workspace split"}
      data-region={region}
    >
      {showMainAgent && (
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
                "flex h-8 shrink-0 cursor-pointer items-center gap-1.5 rounded-lg px-2.5 text-xs font-medium transition-[background-color,color,box-shadow,transform] duration-150 active:translate-y-px",
                FOCUS_RING_TIGHT,
                HOVER_TINT,
                conversationFocused
                  ? "bg-background text-foreground shadow-[0_1px_2px_color-mix(in_oklch,var(--foreground)_7%,transparent),inset_0_0_0_1px_var(--border-subtle)]"
                  : "text-muted-foreground hover:bg-background/65 hover:text-foreground",
              )}
              onClick={onSelectChat}
              onKeyDown={(event) => onKeyDown(event, 0)}
            >
              <MessageSquare className="size-3.5" />
              Main agent
            </button>
          </ContextMenuTrigger>
          <TabContextMenuContent label="Main agent">
            <ContextMenuItem
              className="gap-3 py-2"
              disabled={editorTabs.length === 0}
              onSelect={onCloseEveryEditor ?? onCloseAllEditors}
            >
              <ListX />
              Close other tabs
            </ContextMenuItem>
          </TabContextMenuContent>
        </ContextMenu>
      )}
      <SortableContext
        items={editorTabs.map((panel) => editorTabDragId(region, panel))}
        strategy={horizontalListSortingStrategy}
      >
        {editorTabs.map((panel, index) => (
          <EditorTab
            key={panelKey(panel)}
            panel={panel}
            index={index}
            region={region}
            panelId={panelId}
            active={!conversationFocused && index === editorActiveIndex}
            tabCount={editorTabs.length}
            browserTitles={browserTitles}
            tabRef={(node) => {
              tabRefs.current[index + tabOffset] = node;
            }}
            onSelect={() => onSelectEditor(index)}
            onKeyDown={(event) => onKeyDown(event, index + tabOffset)}
            onClose={() => onCloseEditor(index)}
            onCloseOthers={() => onCloseOtherEditors(index)}
            onCloseToRight={() => onCloseEditorsToRight(index)}
            onCloseAll={onCloseAllEditors}
            onCopyPath={onCopyPath}
            onMoveToOtherGroup={
              onMoveEditorToOtherGroup && (() => onMoveEditorToOtherGroup(index))
            }
            onMove={onMoveEditor && ((to: number) => onMoveEditor(index, to))}
          />
        ))}
      </SortableContext>
      {/* One + for everything the center can open. A bare click must say
          what it will do, so the button offers the choices instead of
          jumping into the file picker. */}
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <button
            type="button"
            className={cn(
              "text-muted-foreground hover:bg-background hover:text-foreground grid size-7 shrink-0 cursor-pointer place-items-center rounded-lg transition-transform active:translate-y-px",
              FOCUS_RING_TIGHT,
              HOVER_TINT,
            )}
            aria-label="New tab"
          >
            <Plus className="size-3.5" />
          </button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="w-44">
          <DropdownMenuItem onSelect={onNewTab}>
            <FileCode />
            Open file…
          </DropdownMenuItem>
          {onNewDiff && (
            <DropdownMenuItem onSelect={onNewDiff}>
              <FileDiff />
              All changes
            </DropdownMenuItem>
          )}
          {onNewSourceControl && (
            <DropdownMenuItem onSelect={onNewSourceControl}>
              <GitBranch />
              Source control
            </DropdownMenuItem>
          )}
          {onNewPr && (
            <DropdownMenuItem onSelect={onNewPr}>
              <GitPullRequest />
              Pull request
            </DropdownMenuItem>
          )}
          {onNewBrowser && (
            <DropdownMenuItem onSelect={onNewBrowser}>
              <Globe2 />
              New browser tab
            </DropdownMenuItem>
          )}
          {onNewTerminal && (
            <DropdownMenuItem onSelect={onNewTerminal}>
              <SquareTerminal />
              Terminal
            </DropdownMenuItem>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
      {region === "primary" && onSplitActive && (
        <button
          type="button"
          className={cn(
            "text-muted-foreground hover:bg-background hover:text-foreground grid size-7 shrink-0 cursor-pointer place-items-center rounded-lg transition-transform active:translate-y-px disabled:cursor-default disabled:opacity-40 disabled:active:translate-y-0",
            FOCUS_RING_TIGHT,
            HOVER_TINT,
          )}
          aria-label="Split active tab right"
          disabled={conversationFocused || editorTabs.length === 0}
          onClick={onSplitActive}
        >
          <Columns2 className="size-3.5" />
        </button>
      )}
      {region === "secondary" && onCloseGroup && (
        <button
          type="button"
          className={cn(
            "text-muted-foreground hover:bg-background hover:text-foreground ml-auto grid size-7 shrink-0 cursor-pointer place-items-center rounded-lg transition-transform active:translate-y-px",
            FOCUS_RING_TIGHT,
            HOVER_TINT,
          )}
          aria-label="Move split tabs to main group"
          onClick={onCloseGroup}
        >
          <PanelRightClose className="size-3.5" />
        </button>
      )}
    </div>
  );
}

/**
 * One draggable tab.
 *
 * Its own component because `useSortable` is a hook, and the strip renders a
 * list. The sortable id is the tab's panel key rather than its position, so a
 * tab keeps its identity while the ones around it shuffle underneath it.
 */
function EditorTab({
  panel,
  index,
  region,
  panelId,
  active,
  tabCount,
  browserTitles,
  tabRef,
  onSelect,
  onKeyDown: onTabKeyDown,
  onClose,
  onCloseOthers,
  onCloseToRight,
  onCloseAll,
  onCopyPath,
  onMoveToOtherGroup,
  onMove,
}: {
  panel: PanelContent;
  index: number;
  region: CenterTabRegion;
  panelId: string;
  active: boolean;
  tabCount: number;
  browserTitles: Readonly<Record<string, string>>;
  tabRef: (node: HTMLButtonElement | null) => void;
  onSelect: () => void;
  onKeyDown: (event: KeyboardEvent<HTMLButtonElement>) => void;
  onClose: () => void;
  onCloseOthers: () => void;
  onCloseToRight: () => void;
  onCloseAll: () => void;
  onCopyPath: (path: string) => void;
  onMoveToOtherGroup?: (() => void) | undefined;
  onMove?: ((to: number) => void) | undefined;
}) {
  const { name, suffix } = centerTabParts(panel, browserTitles);
  const label = suffix ? `${name} ${suffix}` : name;
  const path =
    panel.type === "file" || panel.type === "diff" ? panel.path : undefined;
  // `attributes` is left off on purpose: it turns its element into a focusable
  // button, and this wrapper is presentational — the tab inside it is what the
  // reader focuses. Reordering by keyboard would collide with the tablist's own
  // arrow keys, so the context menu carries that job instead.
  const { listeners, setNodeRef, transform, transition, isDragging } =
    useSortable({ id: editorTabDragId(region, panel) });

  return (
    <ContextMenu>
      <ContextMenuTrigger asChild>
        {/* A tablist owns tabs. The close control is a second control on
            the same row, so this pair is transparent to assistive tech. */}
        <div
          role="presentation"
          ref={setNodeRef}
          style={{ transform: CSS.Translate.toString(transform), transition }}
          className={cn(
            "flex h-8 min-w-0 shrink-0 touch-none cursor-grab items-center rounded-lg pr-1 transition-[background-color,box-shadow,transform] duration-150 active:cursor-grabbing active:translate-y-px",
            HOVER_TINT,
            active
              ? "bg-background shadow-[0_1px_2px_color-mix(in_oklch,var(--foreground)_7%,transparent),inset_0_0_0_1px_var(--border-subtle)]"
              : "hover:bg-background/65",
            // The overlay draws the tab that is moving, so the original stays
            // in place as a gap rather than a second copy of itself.
            isDragging && "opacity-40",
          )}
          {...listeners}
        >
          <button
            type="button"
            role="tab"
            id={editorTabId(region, index)}
            aria-label={label}
            aria-selected={active}
            aria-controls={panelId}
            tabIndex={active ? 0 : -1}
            ref={tabRef}
            className={cn(
              "flex h-full min-w-0 cursor-pointer items-center gap-1.5 rounded-lg pr-1 pl-2.5 text-xs font-medium",
              FOCUS_RING_TIGHT,
              HOVER_TINT,
              active ? "text-foreground" : "text-muted-foreground",
            )}
            onClick={onSelect}
            onKeyDown={onTabKeyDown}
          >
            <CenterTabIcon panel={panel} />
            <span
              className="flex min-w-0 items-baseline gap-1"
              title={centerTabTitle(panel, browserTitles)}
            >
              <span className="max-w-40 truncate">{name}</span>
              {/* The suffix stays outside the truncating name. */}
              {suffix && <span className="shrink-0">{suffix}</span>}
            </span>
          </button>
          <button
            type="button"
            // Pressing close and drifting a few pixels should still close the
            // tab, not carry it somewhere. The sensor reads this flag.
            data-no-drag="true"
            className={cn(
              "text-muted-foreground hover:bg-muted hover:text-foreground grid size-5 shrink-0 cursor-pointer place-items-center rounded-md",
              FOCUS_RING_TIGHT,
              HOVER_TINT,
            )}
            onClick={onClose}
          >
            <X className="size-3" />
            <span className="sr-only">{`Close ${label}`}</span>
          </button>
        </div>
      </ContextMenuTrigger>
      <TabContextMenuContent label={label}>
        {path && (
          <>
            <ContextMenuItem className="gap-3 py-2" onSelect={() => onCopyPath(path)}>
              <Copy />
              Copy path
            </ContextMenuItem>
            <ContextMenuSeparator />
          </>
        )}
        {onMove && (
          <>
            <ContextMenuItem
              className="gap-3 py-2"
              disabled={index === 0}
              onSelect={() => onMove(index - 1)}
            >
              <ChevronLeft />
              Move left
            </ContextMenuItem>
            <ContextMenuItem
              className="gap-3 py-2"
              disabled={index === tabCount - 1}
              onSelect={() => onMove(index + 1)}
            >
              <ChevronRight />
              Move right
            </ContextMenuItem>
            <ContextMenuSeparator />
          </>
        )}
        {onMoveToOtherGroup && (
          <>
            <ContextMenuItem className="gap-3 py-2" onSelect={onMoveToOtherGroup}>
              {region === "primary" ? <MoveRight /> : <MoveLeft />}
              {region === "primary" ? "Move to split right" : "Move to main group"}
            </ContextMenuItem>
            <ContextMenuSeparator />
          </>
        )}
        <ContextMenuItem className="gap-3 py-2" onSelect={onClose}>
          <X />
          Close tab
        </ContextMenuItem>
        <ContextMenuItem
          className="gap-3 py-2"
          disabled={tabCount <= 1}
          onSelect={onCloseOthers}
        >
          <ListX />
          Close other tabs
        </ContextMenuItem>
        <ContextMenuItem
          className="gap-3 py-2"
          disabled={index === tabCount - 1}
          onSelect={onCloseToRight}
        >
          <ArrowRightFromLine />
          Close tabs to the right
        </ContextMenuItem>
        <ContextMenuItem className="gap-3 py-2" onSelect={onCloseAll}>
          <CircleX />
          Close all tabs
        </ContextMenuItem>
      </TabContextMenuContent>
    </ContextMenu>
  );
}

/** The mark that says what kind of thing a tab holds. */
export function CenterTabIcon({ panel }: { panel: PanelContent }) {
  if (panel.type === "diff") return <FileDiff className="size-3.5 shrink-0" />;
  if (panel.type === "browser") return <Globe2 className="size-3.5 shrink-0" />;
  if (panel.type === "source_control") {
    return <GitBranch className="size-3.5 shrink-0" />;
  }
  if (panel.type === "pr") {
    return <GitPullRequest className="size-3.5 shrink-0" />;
  }
  return <FileCode className="size-3.5 shrink-0" />;
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
function centerTabTitle(
  panel: PanelContent,
  browserTitles: Readonly<Record<string, string>>,
): string {
  if (panel.type === "file") return panel.path;
  if (panel.type === "diff" && panel.path) return `${panel.path} (diff)`;
  const { name, suffix } = centerTabParts(panel, browserTitles);
  return suffix ? `${name} ${suffix}` : name;
}

/**
 * A tab's label, split into the part that may truncate and the part that may
 * not.
 */
export function centerTabParts(
  panel: PanelContent,
  browserTitles: Readonly<Record<string, string>>,
): {
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
  if (panel.type === "browser") {
    return {
      name: browserTitles[panel.browserId]?.trim() || "Browser",
      suffix: null,
    };
  }
  if (panel.type === "source_control") {
    return { name: "Source control", suffix: null };
  }
  if (panel.type === "pr") {
    return { name: "Pull request", suffix: null };
  }
  return { name: panel.type, suffix: null };
}
