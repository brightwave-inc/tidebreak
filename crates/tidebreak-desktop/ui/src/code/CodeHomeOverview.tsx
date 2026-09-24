import {
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
  type Ref,
} from "react";
import { useNavigate } from "@tanstack/react-router";
import { formatDistanceToNowStrict } from "date-fns";
import {
  Circle,
  CircleAlert,
  CircleCheck,
  Copy,
  GitBranch,
  GitMerge,
  GitPullRequest,
  GitPullRequestClosed,
  GitPullRequestDraft,
  Plus,
  Settings2,
} from "lucide-react";
import { toast } from "sonner";

import type { CodeRepoSnapshot } from "../api/types";
import { copyPlainText } from "@/ClipboardCopyButton";
import { Loader } from "@/components/motion/loader";
import { Button } from "@/components/ui/button";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import {
  HoverCard,
  HoverCardContent,
  HoverCardTrigger,
} from "@/components/ui/hover-card";
import { LiveLabel } from "@/LiveLabel";
import { cn } from "@/lib/utils";
import {
  CODE_HOME_SECTION_LABELS,
  CODE_HOME_SECTION_LIMIT,
  CODE_HOME_SECTION_ORDER,
  type CodeHomeActivity,
  type CodeHomeGlyph,
  type CodeHomeItem,
  type CodeHomeSectionId,
  type CodeHomeSections,
  type CodeHomeTarget,
} from "./codeHomeSections";
import { FOCUS_RING_INSET, HOVER_TINT } from "./interactive";
import { MiddleTruncate } from "./MiddleTruncate";
import { PULL_REQUEST_LIFECYCLE_TONE } from "./prState";
import { STATUS_MARK, STATUS_TEXT } from "./statusTone";
import { SessionStateGlyph } from "./WorkspaceCard";
import { formatCompactAge, repoAccentClass } from "./workspaceCards";

/**
 * The work half of the Code home: what needs you, what is running, what is
 * ready to merge, and recent work, each capped with a View all.
 *
 * Rows are plain buttons that open the right place — the workspace, the
 * conversation, or the pull request in Delivery. Nothing on a row is a
 * second control, and nothing here reads the network: the sections come in
 * already derived from the stores the rail reads (`codeHomeSections`).
 */
export function CodeHomeWork({
  sections,
  snapshotLoaded,
  onNewWorkspace,
}: {
  sections: CodeHomeSections;
  /** Until the live snapshot lands, the page cannot say nothing needs you. */
  snapshotLoaded: boolean;
  onNewWorkspace: () => void;
}) {
  const openTarget = useOpenCodeHomeTarget();
  if (sections.total === 0) {
    return <NoWorkspaces onNewWorkspace={onNewWorkspace} />;
  }
  return (
    <div className="flex flex-col gap-8">
      {CODE_HOME_SECTION_ORDER.map((id) => {
        if (sections[id].length > 0) {
          return (
            <HomeSection
              key={id}
              id={id}
              items={sections[id]}
              onOpen={openTarget}
            />
          );
        }
        // The page's first question always gets an answer, once it has one.
        if (id === "needs_you" && snapshotLoaded) {
          return <NothingNeedsYou key={id} />;
        }
        return null;
      })}
    </div>
  );
}

function useOpenCodeHomeTarget(): (target: CodeHomeTarget) => void {
  const navigate = useNavigate();
  return (target) => {
    switch (target.kind) {
      case "workspace":
        void navigate({
          to: "/code/w/$workspaceId",
          params: { workspaceId: target.workspaceId },
          ...(target.task ? { search: { task: target.task } } : {}),
        });
        return;
      case "session":
        void navigate({
          to: "/code/s/$sessionId",
          params: { sessionId: target.sessionId },
        });
        return;
      case "pull_request":
        void navigate({
          to: "/code/delivery/pull-requests",
          search: {
            repoHost: target.repository.host,
            repoOwner: target.repository.owner,
            repoName: target.repository.name,
            pr: target.number,
          },
        });
    }
  };
}

function HomeSection({
  id,
  items,
  onOpen,
}: {
  id: CodeHomeSectionId;
  items: readonly CodeHomeItem[];
  onOpen: (target: CodeHomeTarget) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const firstRevealed = useRef<HTMLButtonElement>(null);
  const revealing = useRef(false);
  const capped = items.length > CODE_HOME_SECTION_LIMIT;
  const shown =
    capped && !expanded ? items.slice(0, CODE_HOME_SECTION_LIMIT) : items;
  const headingId = `code-home-${id}`;

  // View all lands the keyboard on the first row it revealed, so the reader
  // carries on down the list instead of starting over from the button.
  useEffect(() => {
    if (!expanded || !revealing.current) return;
    revealing.current = false;
    firstRevealed.current?.focus();
  }, [expanded]);

  return (
    <section aria-labelledby={headingId} className="flex flex-col gap-2.5">
      <SectionHeading id={headingId} count={items.length}>
        {CODE_HOME_SECTION_LABELS[id]}
      </SectionHeading>
      <ul
        className="divide-y divide-border-subtle overflow-hidden rounded-xl border border-border"
        data-code-home-section={id}
      >
        {shown.map((item, index) => (
          <li key={item.key}>
            <HomeRow
              item={item}
              buttonRef={
                index === CODE_HOME_SECTION_LIMIT ? firstRevealed : undefined
              }
              onOpen={() => onOpen(item.target)}
            />
          </li>
        ))}
        {capped && (
          <li>
            <button
              type="button"
              aria-expanded={expanded}
              aria-label={
                expanded
                  ? `Show fewer in ${CODE_HOME_SECTION_LABELS[id]}`
                  : `View all ${items.length} in ${CODE_HOME_SECTION_LABELS[id]}`
              }
              className={cn(
                // Indented past the glyph column so it lines up with titles.
                "w-full cursor-pointer py-2 pr-3.5 pl-[2.375rem] text-left text-xs font-medium text-muted-foreground hover:bg-muted/50 hover:text-foreground",
                FOCUS_RING_INSET,
                HOVER_TINT,
              )}
              onClick={() => {
                revealing.current = !expanded;
                setExpanded((open) => !open);
              }}
            >
              {expanded ? "Show fewer" : `View all ${items.length}`}
            </button>
          </li>
        )}
      </ul>
    </section>
  );
}

function SectionHeading({
  id,
  count,
  children,
  action,
}: {
  id: string;
  count?: number;
  children: string;
  action?: ReactNode;
}) {
  return (
    <div className="flex min-h-control-sm items-center justify-between gap-3">
      <h2 id={id} className="flex items-baseline gap-2 text-sm font-semibold">
        {children}
        {count !== undefined && (
          <>
            {/* Read as "Needs you, 7", not "Needs you7". The space is its
                own text node: flex layout drops it, the name keeps it. */}
            <span className="sr-only">,</span>{" "}
            <span className="font-normal text-muted-foreground tabular-nums">
              {count}
            </span>
          </>
        )}
      </h2>
      {action}
    </div>
  );
}

/**
 * The row rhythm every list on the page shares: a glyph column, the title,
 * and a trailing column. The quiet state and View all line up on it too.
 */
const ROW_GRID =
  "grid grid-cols-[0.75rem_minmax(0,1fr)_auto] gap-x-3 px-3.5 py-2.5";

/**
 * One item: a glyph and a title, then why it is here, its repository, and
 * the branch or pull request that tells it apart. The branch keeps its tail,
 * because the tail is what differs between two branches off one prefix.
 */
function HomeRow({
  item,
  buttonRef,
  onOpen,
}: {
  item: CodeHomeItem;
  buttonRef?: Ref<HTMLButtonElement>;
  onOpen: () => void;
}) {
  const age = item.activity ? formatCompactAge(item.activity.at) : null;
  const when = activityPhrase(item.activity);
  const reference =
    item.reference?.kind === "pull_request"
      ? `#${item.reference.number}`
      : (item.reference?.name ?? null);
  return (
    <button
      ref={buttonRef}
      type="button"
      aria-label={[
        item.title,
        item.status?.label,
        item.context,
        reference,
        when?.toLowerCase(),
      ]
        .filter(Boolean)
        .join(" · ")}
      className={cn(
        ROW_GRID,
        "w-full cursor-pointer text-left hover:bg-muted/50",
        FOCUS_RING_INSET,
        HOVER_TINT,
      )}
      onClick={onOpen}
    >
      <span className="flex h-5 items-center" aria-hidden>
        <HomeGlyph glyph={item.glyph} />
      </span>
      <span className="min-w-0 truncate text-md font-medium leading-5">
        {item.title}
      </span>
      <span
        className="text-xs leading-5 text-muted-foreground tabular-nums"
        title={when ?? undefined}
      >
        {age}
      </span>
      <span
        className="col-start-2 col-span-2 mt-0.5 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground"
        aria-hidden
      >
        {item.status &&
          (item.status.live ? (
            <LiveLabel live className="block min-w-0 truncate">
              {item.status.label}
            </LiveLabel>
          ) : (
            <span
              className={cn("min-w-0 truncate", STATUS_TEXT[item.status.tone])}
            >
              {item.status.label}
            </span>
          ))}
        {item.context && (
          <>
            {item.status && <Separator />}
            <span className="min-w-0 truncate">{item.context}</span>
          </>
        )}
        {reference && (
          <span className="flex min-w-0 flex-1 items-center gap-1.5 overflow-hidden">
            {(item.status || item.context) && <Separator />}
            {item.reference?.kind === "branch" ? (
              <MiddleTruncate text={reference} className="min-w-0 font-mono" />
            ) : (
              <span className="shrink-0 tabular-nums">{reference}</span>
            )}
          </span>
        )}
      </span>
    </button>
  );
}

function Separator() {
  return (
    <span className="shrink-0" aria-hidden>
      ·
    </span>
  );
}

/**
 * The age in words. A workspace nothing has run in yet has only its creation
 * time, and says so rather than passing it off as activity.
 */
function activityPhrase(activity: CodeHomeActivity | null): string | null {
  if (!activity) return null;
  const at = new Date(activity.at);
  if (Number.isNaN(at.getTime())) return null;
  const ago = formatDistanceToNowStrict(at, { addSuffix: true });
  return activity.kind === "created"
    ? `Created ${ago}`
    : `Last activity ${ago}`;
}

function HomeGlyph({ glyph }: { glyph: CodeHomeGlyph }) {
  switch (glyph.kind) {
    case "session":
      return <SessionStateGlyph digest={glyph.digest} />;
    case "pull_request": {
      const Icon =
        glyph.lifecycle === "merged"
          ? GitMerge
          : glyph.lifecycle === "closed"
            ? GitPullRequestClosed
            : glyph.lifecycle === "draft"
              ? GitPullRequestDraft
              : GitPullRequest;
      // The rail's own mark: the lifecycle paints it, and the row's text
      // carries the gate (checks, review, conflicts).
      return (
        <Icon
          className={cn(
            "size-3 shrink-0",
            STATUS_MARK[PULL_REQUEST_LIFECYCLE_TONE[glyph.lifecycle]],
          )}
          data-pr-state={glyph.lifecycle}
        />
      );
    }
    case "alert":
      return (
        <CircleAlert
          className={cn("size-3 shrink-0", STATUS_MARK[glyph.tone])}
        />
      );
    case "live":
      return (
        <Loader variant="comet" size={12} className="text-live" decorative />
      );
    case "idle":
      return <Circle className="size-3 shrink-0 text-muted-foreground" />;
  }
}

/** The Needs you slot once the live snapshot says nothing is waiting. */
function NothingNeedsYou() {
  return (
    <section
      aria-labelledby="code-home-needs_you"
      className="flex flex-col gap-2.5"
    >
      <SectionHeading id="code-home-needs_you">Needs you</SectionHeading>
      <div className={cn(ROW_GRID, "rounded-xl border border-border")}>
        <span className="flex h-5 items-center" aria-hidden>
          <CircleCheck className="size-3 shrink-0 text-success" />
        </span>
        <p className="col-span-2 min-w-0 text-md leading-5">
          Nothing needs you right now.
          <span className="mt-0.5 block text-xs text-muted-foreground">
            Approvals, questions, and failed checks show up here.
          </span>
        </p>
      </div>
    </section>
  );
}

function NoWorkspaces({ onNewWorkspace }: { onNewWorkspace: () => void }) {
  return (
    <Empty className="flex-none py-10 md:py-12">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <GitBranch aria-hidden="true" />
        </EmptyMedia>
        <EmptyTitle>No workspaces yet</EmptyTitle>
        <EmptyDescription>
          A workspace gives one task its own branch and folder. Start one on any
          repository below.
        </EmptyDescription>
      </EmptyHeader>
      <EmptyContent>
        <Button type="button" size="sm" onClick={onNewWorkspace}>
          <Plus aria-hidden="true" />
          New workspace
        </Button>
      </EmptyContent>
    </Empty>
  );
}

/**
 * The registered repositories, second to the work on them.
 *
 * A row starts a workspace on its repository. Everything else lives where
 * the rail keeps it: a hover card for the details and the menu on
 * right-click, which Shift+F10 and the context-menu key also open, so the
 * keyboard reaches the same commands. Each row says so to assistive
 * technology, since nothing on it shows the menu.
 */
export function CodeHomeRepositories({
  repos,
  liveWorkspaceCounts,
  onAddRepo,
  onNewWorkspace,
  onOpenSettings,
}: {
  repos: readonly CodeRepoSnapshot[];
  liveWorkspaceCounts: Readonly<Record<string, number>>;
  onAddRepo: () => void;
  onNewWorkspace: (repoId: string) => void;
  /** `origin` is the row that asked, for focus to return to. */
  onOpenSettings: (repo: CodeRepoSnapshot, origin: HTMLElement | null) => void;
}) {
  const menuHintId = useId();
  return (
    <section
      aria-labelledby="code-home-repositories"
      className="flex flex-col gap-2.5"
    >
      <SectionHeading
        id="code-home-repositories"
        count={repos.length}
        action={
          <Button type="button" size="sm" variant="ghost" onClick={onAddRepo}>
            <Plus aria-hidden="true" />
            Add repo
          </Button>
        }
      >
        Repositories
      </SectionHeading>
      {repos.length === 0 ? (
        <p className="rounded-xl border border-border px-3.5 py-3 text-sm text-muted-foreground">
          No repository is registered on this machine yet.
        </p>
      ) : (
        <>
          <span id={menuHintId} className="sr-only">
            More actions: Shift+F10
          </span>
          <ul className="divide-y divide-border-subtle overflow-hidden rounded-xl border border-border">
            {repos.map((repo) => (
              <li key={repo.id}>
                <RepositoryRow
                  repo={repo}
                  liveWorkspaces={liveWorkspaceCounts[repo.id] ?? 0}
                  menuHintId={menuHintId}
                  onNewWorkspace={() => onNewWorkspace(repo.id)}
                  onOpenSettings={(origin) => onOpenSettings(repo, origin)}
                />
              </li>
            ))}
          </ul>
        </>
      )}
    </section>
  );
}

function RepositoryRow({
  repo,
  liveWorkspaces,
  menuHintId,
  onNewWorkspace,
  onOpenSettings,
}: {
  repo: CodeRepoSnapshot;
  liveWorkspaces: number;
  menuHintId: string;
  onNewWorkspace: () => void;
  onOpenSettings: (origin: HTMLElement | null) => void;
}) {
  const rowRef = useRef<HTMLButtonElement>(null);
  const [detailOpen, setDetailOpen] = useState(false);
  const [detailSide, setDetailSide] = useState<RepositoryDetailSide>("right");
  const copyPath = () => {
    void copyPlainText(repo.root_path)
      .then(() => toast.success("Repository path copied"))
      .catch(() => toast.error("Could not copy the repository path"));
  };
  return (
    <ContextMenu
      onOpenChange={(open) => {
        if (open) setDetailOpen(false);
      }}
    >
      <HoverCard
        open={detailOpen}
        onOpenChange={(open) => {
          if (open) setDetailSide(repositoryDetailSide(rowRef.current));
          setDetailOpen(open);
        }}
        openDelay={400}
        closeDelay={120}
      >
        <ContextMenuTrigger asChild>
          <HoverCardTrigger asChild>
            <button
              ref={rowRef}
              type="button"
              aria-label={`New workspace on ${repo.display_name}`}
              aria-describedby={menuHintId}
              aria-keyshortcuts="Shift+F10"
              data-repository-row={repo.id}
              className={cn(
                ROW_GRID,
                "w-full cursor-pointer items-center text-left hover:bg-muted/50",
                FOCUS_RING_INSET,
                HOVER_TINT,
              )}
              onClick={onNewWorkspace}
              onKeyDown={openRowMenuFromKeyboard}
            >
              <span className="flex justify-center" aria-hidden>
                <span
                  className={cn(
                    "size-2 rounded-[3px]",
                    repoAccentClass(repo.id),
                  )}
                />
              </span>
              <span className="flex min-w-0 items-baseline gap-3">
                <span className="max-w-[45%] shrink-0 truncate text-md font-medium leading-5">
                  {repo.display_name}
                </span>
                <MiddleTruncate
                  text={repo.root_path}
                  className="min-w-0 flex-1 font-mono text-xs text-muted-foreground"
                />
              </span>
              <span className="text-xs text-muted-foreground tabular-nums">
                {liveWorkspaces > 0 ? workspaceCount(liveWorkspaces) : null}
              </span>
            </button>
          </HoverCardTrigger>
        </ContextMenuTrigger>
        <HoverCardContent
          side={detailSide}
          // Beside the row, like the rail's card. Where the window leaves no
          // room beside the list, it hangs from the row's far end, clear of
          // the names the pointer runs down.
          align={detailSide === "right" ? "start" : "end"}
          sideOffset={detailSide === "right" ? DETAIL_GAP_PX : 6}
          className="w-[min(22rem,calc(100vw-24px))] overflow-hidden rounded-xl border-border bg-popover p-0"
        >
          <RepositoryDetail
            repo={repo}
            liveWorkspaces={liveWorkspaces}
            onNewWorkspace={() => {
              setDetailOpen(false);
              onNewWorkspace();
            }}
            onOpenSettings={() => {
              setDetailOpen(false);
              onOpenSettings(rowRef.current);
            }}
          />
        </HoverCardContent>
      </HoverCard>
      <ContextMenuContent>
        <ContextMenuItem onSelect={onNewWorkspace}>
          <Plus aria-hidden="true" />
          New workspace
        </ContextMenuItem>
        <ContextMenuItem onSelect={() => onOpenSettings(rowRef.current)}>
          <Settings2 aria-hidden="true" />
          Repository settings…
        </ContextMenuItem>
        <ContextMenuItem onSelect={copyPath}>
          <Copy aria-hidden="true" />
          Copy path
        </ContextMenuItem>
      </ContextMenuContent>
    </ContextMenu>
  );
}

type RepositoryDetailSide = "right" | "bottom";

/** The card's width in rem, matching its `w-[min(22rem,…)]` class. */
const DETAIL_WIDTH_REM = 22;
const DETAIL_GAP_PX = 10;
/** Room the card keeps from the window edge. */
const DETAIL_EDGE_PX = 12;

/**
 * Right of the row when the window has room for the whole card there;
 * otherwise below it. Measured when the card opens, because the room depends
 * on the window, the rail, and where the list sits between them.
 */
export function repositoryDetailSide(
  row: HTMLElement | null,
): RepositoryDetailSide {
  if (!row || typeof window === "undefined") return "bottom";
  const rem =
    Number.parseFloat(getComputedStyle(document.documentElement).fontSize) ||
    14;
  const room = window.innerWidth - row.getBoundingClientRect().right;
  return room >= DETAIL_WIDTH_REM * rem + DETAIL_GAP_PX + DETAIL_EDGE_PX
    ? "right"
    : "bottom";
}

function RepositoryDetail({
  repo,
  liveWorkspaces,
  onNewWorkspace,
  onOpenSettings,
}: {
  repo: CodeRepoSnapshot;
  liveWorkspaces: number;
  onNewWorkspace: () => void;
  onOpenSettings: () => void;
}) {
  const prefix = repo.branch_prefix.trim();
  return (
    <div data-testid="repository-hover-card">
      <div className="flex flex-col gap-2 p-3.5">
        <p className="text-base font-semibold leading-5">{repo.display_name}</p>
        <p className="font-mono text-xs break-all text-muted-foreground">
          {repo.root_path}
        </p>
        <dl className="mt-1 grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 text-xs">
          <dt className="text-muted-foreground">Base branch</dt>
          <dd className="truncate font-mono">{repo.default_base_ref}</dd>
          {prefix && (
            <>
              <dt className="text-muted-foreground">Branch prefix</dt>
              <dd className="truncate font-mono">{prefix}/</dd>
            </>
          )}
          <dt className="text-muted-foreground">Workspaces</dt>
          <dd>{liveWorkspaces > 0 ? `${liveWorkspaces} open` : "None open"}</dd>
        </dl>
      </div>
      <div className="flex flex-wrap items-center gap-1 border-t border-border-subtle bg-muted/25 px-2 py-2">
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-7 px-1.5"
          onClick={onNewWorkspace}
        >
          <Plus aria-hidden="true" />
          New workspace
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          className="h-7 px-1.5"
          onClick={onOpenSettings}
        >
          <Settings2 aria-hidden="true" />
          Settings
        </Button>
      </div>
    </div>
  );
}

function workspaceCount(count: number): string {
  return count === 1 ? "1 workspace" : `${count} workspaces`;
}

/**
 * Open a row's context menu from the keyboard. Shift+F10 and the
 * context-menu key are the platform's own gesture for it; the menu opens
 * under the row, where a pointer would have put it.
 */
function openRowMenuFromKeyboard(event: KeyboardEvent<HTMLElement>): void {
  if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) {
    return;
  }
  event.preventDefault();
  const row = event.currentTarget;
  const bounds = row.getBoundingClientRect();
  row.dispatchEvent(
    new MouseEvent("contextmenu", {
      bubbles: true,
      cancelable: true,
      clientX: bounds.left + 24,
      clientY: bounds.bottom,
    }),
  );
}
