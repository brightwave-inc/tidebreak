import { Button } from "@/components/ui/button";
import type { CodeDeliveryPullRequestSummary } from "../../api/types";
import { CornerDownRight, LoaderCircle, MessageSquare } from "lucide-react";
import { GithubAvatar } from "../GithubAvatar";
import {
  PR_GRID,
  PR_GROUP_HEIGHT,
  PR_ROW_HEIGHT,
  VirtualRows,
} from "./VirtualRows";
import { PrLifecycleIcon, relativeTime } from "../PullRequestDetail";
import { PrCheckSummary } from "../PrCheckSummary";
import type { PullRequestGrouping } from "./views";
import { checkCounts, prStatus, type PullRequestListGroup } from "../prState";
import {
  STATUS_DOT,
  STATUS_MARK,
  STATUS_TEXT,
  type StatusTone,
} from "../statusTone";
import {
  arrangeStackLanes,
  isStackedPullRequest,
  type StackedRow,
} from "../pullRequestStacks";
import { cn } from "@/lib/utils";
import { codeDeliveryRepositoryKey } from "../CodeDeliveryStore";
import {
  deliveryPullRequestDigest,
  deliveryRepositoryHasMergeQueue,
  prDirectMergeAction,
} from "../prActions";
import { useMemo } from "react";

export type PullRequestListItem =
  | {
      id: string;
      kind: "group";
      label: string;
      description: string;
      tone: StatusTone;
      count: number;
    }
  | {
      id: string;
      kind: "pull_request";
      row: StackedRow;
      showRepository: boolean;
    };

const PULL_REQUEST_GROUP_ORDER: readonly PullRequestListGroup[] = [
  "attention",
  "ready",
  "waiting",
  "handed_off",
  "draft",
  "done",
];

const PULL_REQUEST_GROUP_RANK = new Map(
  PULL_REQUEST_GROUP_ORDER.map((group, index) => [group, index]),
);

const PULL_REQUEST_GROUP_META: Record<
  PullRequestListGroup,
  { label: string; description: string; tone: StatusTone }
> = {
  attention: {
    label: "Needs your attention",
    description:
      "Failed checks, requested changes, conflicts, or stale branches",
    tone: "critical",
  },
  ready: {
    label: "Ready to merge",
    description: "Green and waiting for you",
    tone: "ready",
  },
  waiting: {
    label: "Waiting",
    description: "Checks or reviews are still moving",
    tone: "pending",
  },
  handed_off: {
    label: "Handed off",
    description: "Auto-merge is armed or GitHub has queued the merge",
    tone: "pending",
  },
  draft: {
    label: "Drafts",
    description: "Not ready for review",
    tone: "neutral",
  },
  done: {
    label: "Done",
    description: "Merged or closed",
    tone: "merged",
  },
};

export function PullRequestList({
  items,
  grouping,
  selectedId,
  busyId,
  onSelect,
  onMerge,
  scrollRef,
}: {
  items: CodeDeliveryPullRequestSummary[];
  grouping: PullRequestGrouping;
  selectedId: string | null;
  busyId: string | null;
  onSelect: (id: string) => void;
  onMerge: (item: CodeDeliveryPullRequestSummary) => void;
  scrollRef: React.RefObject<HTMLDivElement | null>;
}) {
  const rows = useMemo(
    () => groupedPullRequestRows(items, grouping),
    [grouping, items],
  );
  return (
    <div role="list" aria-label="Pull requests" className="min-w-[1040px]">
      <div
        className={cn(
          "sticky top-0 z-10 grid gap-4 border-b border-border-subtle bg-background/95 px-5 py-2 text-xs font-medium text-muted-foreground backdrop-blur",
          PR_GRID,
        )}
      >
        <span>Pull request</span>
        <span>Status</span>
        <span>Checks</span>
        <span>Comments</span>
        <span>Action</span>
        <span className="text-right">Updated</span>
      </div>
      <VirtualRows
        items={rows}
        scrollRef={scrollRef}
        scrollToId={selectedId}
        estimateSize={(item) =>
          item.kind === "group" ? PR_GROUP_HEIGHT : PR_ROW_HEIGHT
        }
      >
        {(entry) =>
          entry.kind === "group" ? (
            <PullRequestGroupHeader {...entry} />
          ) : (
            <PullRequestRow
              item={entry.row.item}
              depth={entry.row.depth}
              stackedOn={entry.row.stackedOn}
              showRepository={entry.showRepository}
              active={selectedId === entry.row.item.id}
              busy={busyId === entry.row.item.id}
              hasMergeQueue={deliveryRepositoryHasMergeQueue(
                items,
                entry.row.item.repository,
              )}
              onSelect={() => onSelect(entry.row.item.id)}
              onMerge={() => onMerge(entry.row.item)}
            />
          )
        }
      </VirtualRows>
    </div>
  );
}

export function groupedPullRequestRows(
  items: readonly CodeDeliveryPullRequestSummary[],
  grouping: PullRequestGrouping,
): PullRequestListItem[] {
  if (grouping === "none") {
    return arrangeStackLanes(items).map((row) => ({
      id: row.id,
      kind: "pull_request",
      row,
      showRepository: true,
    }));
  }

  if (grouping === "repository") {
    const repositories = new Map<
      string,
      { label: string; items: CodeDeliveryPullRequestSummary[] }
    >();
    for (const item of items) {
      const key = codeDeliveryRepositoryKey(item.repository);
      const group = repositories.get(key) ?? {
        label: item.repository.name_with_owner,
        items: [],
      };
      group.items.push(item);
      repositories.set(key, group);
    }
    return [...repositories.entries()].flatMap(([key, group]) => {
      const attention = group.items.filter(
        (item) => prStatus(item).group === "attention",
      ).length;
      return pullRequestGroupRows({
        key: `repository:${key}`,
        label: group.label,
        description:
          attention > 0
            ? `${attention} ${attention === 1 ? "pull request needs" : "pull requests need"} attention`
            : "No pull requests need attention",
        tone: attention > 0 ? "critical" : "neutral",
        rows: arrangeStackLanes(group.items),
        showRepository: false,
      });
    });
  }

  const groups = new Map<PullRequestListGroup, StackedRow[]>();
  for (const lane of pullRequestStackLanes(items)) {
    const group = lane.reduce<PullRequestListGroup>((mostUrgent, row) => {
      const candidate = prStatus(row.item).group;
      return (PULL_REQUEST_GROUP_RANK.get(candidate) ??
        Number.MAX_SAFE_INTEGER) <
        (PULL_REQUEST_GROUP_RANK.get(mostUrgent) ?? Number.MAX_SAFE_INTEGER)
        ? candidate
        : mostUrgent;
    }, "done");
    const grouped = groups.get(group) ?? [];
    grouped.push(...lane);
    groups.set(group, grouped);
  }
  return PULL_REQUEST_GROUP_ORDER.flatMap((group) => {
    const grouped = groups.get(group);
    if (!grouped?.length) return [];
    const meta = PULL_REQUEST_GROUP_META[group];
    return pullRequestGroupRows({
      key: `attention:${group}`,
      ...meta,
      rows: grouped,
      showRepository: true,
    });
  });
}

/** Keep a stack in one attention group so its indentation still explains it. */
function pullRequestStackLanes(
  items: readonly CodeDeliveryPullRequestSummary[],
): StackedRow[][] {
  const lanes: StackedRow[][] = [];
  for (const row of arrangeStackLanes(items)) {
    if (row.depth === 0 || lanes.length === 0) lanes.push([row]);
    else lanes[lanes.length - 1]!.push(row);
  }
  return lanes;
}

function pullRequestGroupRows({
  key,
  label,
  description,
  tone,
  rows,
  showRepository,
}: {
  key: string;
  label: string;
  description: string;
  tone: StatusTone;
  rows: readonly StackedRow[];
  showRepository: boolean;
}): PullRequestListItem[] {
  return [
    {
      id: `group:${key}`,
      kind: "group",
      label,
      description,
      tone,
      count: rows.length,
    },
    ...rows.map(
      (row): PullRequestListItem => ({
        id: row.id,
        kind: "pull_request",
        row,
        showRepository,
      }),
    ),
  ];
}

function PullRequestGroupHeader({
  label,
  description,
  tone,
  count,
}: {
  label: string;
  description: string;
  tone: StatusTone;
  count: number;
}) {
  return (
    <div
      data-pull-request-group={label}
      className="flex items-center gap-2 border-b border-border-subtle bg-muted/20 px-5 py-2.5 text-xs"
    >
      <span
        className={cn("size-1.5 shrink-0 rounded-full", STATUS_DOT[tone])}
        aria-hidden
      />
      <span className="font-semibold text-foreground">{label}</span>
      <span className="truncate text-muted-foreground">{description}</span>
      <span className="ml-auto shrink-0 tabular-nums text-muted-foreground">
        {count}
      </span>
    </div>
  );
}

function PullRequestRow({
  item,
  depth,
  stackedOn,
  showRepository,
  active,
  busy,
  hasMergeQueue,
  onSelect,
  onMerge,
}: {
  item: CodeDeliveryPullRequestSummary;
  depth: number;
  stackedOn?: number;
  showRepository: boolean;
  active: boolean;
  busy: boolean;
  hasMergeQueue: boolean;
  onSelect: () => void;
  onMerge: () => void;
}) {
  const status = prStatus(item);
  const lifecycle = status.lifecycle;
  const checks = checkCounts(item);
  const comments = item.comment_count;
  const mergeAction = item.head_sha
    ? prDirectMergeAction(deliveryPullRequestDigest(item), {
        hasMergeQueue,
        suppressAutoMerge: isStackedPullRequest(item),
      })
    : null;
  return (
    <div
      role="listitem"
      tabIndex={0}
      data-active={active || undefined}
      data-pull-request-id={item.id}
      data-depth={depth}
      data-status-group={status.group}
      className={cn(
        "grid w-full cursor-pointer gap-4 border-b border-border-subtle px-5 py-3 text-left transition-colors hover:bg-muted/35 data-[active]:bg-muted/55",
        PR_GRID,
      )}
      onClick={onSelect}
      onKeyDown={(event) => {
        if (event.target !== event.currentTarget) return;
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onSelect();
        }
      }}
    >
      <span
        className="min-w-0"
        style={depth > 0 ? { paddingLeft: depth * 16 } : undefined}
      >
        <span className="flex min-w-0 items-center gap-2">
          {depth > 0 && (
            <CornerDownRight
              className="size-3.5 shrink-0 text-muted-foreground/70"
              aria-label={`Stacked on the pull request above, level ${depth}`}
            />
          )}
          <PrLifecycleIcon
            lifecycle={lifecycle}
            className={cn("size-4", STATUS_MARK[status.headline.tone])}
          />
          <span className="sr-only">{status.headline.label}:</span>
          <span className="truncate text-sm font-medium">{item.title}</span>
        </span>
        <span className="mt-1 flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
          {item.author && (
            // Leading the metadata line rather than taking a column: the
            // avatars line up as their own strip down the list, and the table
            // keeps the width it has.
            <>
              <GithubAvatar
                login={item.author}
                url={item.author_avatar_url}
                className="size-4"
              />
              {/* The login holds its width while the repository and branch
                  give theirs up: a login truncated to one letter identifies
                  nobody, and those two read fine clipped. */}
              <span className="max-w-32 shrink-0 truncate">{item.author}</span>
              <span className="shrink-0" aria-hidden>
                ·
              </span>
            </>
          )}
          {showRepository && (
            <span className="truncate">{item.repository.name_with_owner}</span>
          )}
          <span className="tabular-nums">#{item.number}</span>
          <span className="truncate font-mono">{item.head_branch}</span>
          {stackedOn !== undefined && (
            <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-2xs tabular-nums">
              Stacked on #{stackedOn}
            </span>
          )}
          {item.unregistered_stack_numbers !== undefined && (
            <span
              className="text-info-foreground-muted shrink-0 rounded bg-info-background px-1.5 py-0.5 text-2xs"
              title="This chain is not registered as a GitHub stack. Create the stack on the pull request page so GitHub owns the ordering and the whole-chain merge."
            >
              Unregistered stack
            </span>
          )}
          {item.workspace_links.length > 0 && (
            <span className="shrink-0 rounded bg-info-background px-1.5 py-0.5 text-2xs text-info-foreground-muted">
              Tidebreak
            </span>
          )}
        </span>
      </span>
      <span className="flex items-center">
        <span
          className={cn(
            "text-xs font-medium",
            STATUS_TEXT[status.headline.tone],
          )}
        >
          {status.headline.label}
        </span>
      </span>
      <span className="flex items-center">
        <PrCheckSummary counts={checks} />
      </span>
      <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <MessageSquare className="size-3.5 shrink-0" />
        <span className="tabular-nums">
          {comments === undefined
            ? "—"
            : comments === 0
              ? "None"
              : `${comments} ${comments === 1 ? "comment" : "comments"}`}
        </span>
      </span>
      <span className="flex items-center">
        {mergeAction ? (
          <Button
            type="button"
            size="xs"
            variant={mergeAction.kind === "merge" ? "default" : "outline"}
            disabled={busy}
            onClick={(event) => {
              event.stopPropagation();
              onMerge();
            }}
          >
            {busy && <LoaderCircle className="animate-spin" />}
            {mergeAction.label}
          </Button>
        ) : null}
      </span>
      <span className="flex items-center justify-end text-xs text-muted-foreground">
        {relativeTime(item.updated_at)}
      </span>
    </div>
  );
}
