import type { CodeDeliveryPullRequestSummary } from "../api/types";

/** Preserve list-known stack facts when a detail read omits enrichment. */
export function preservePullRequestStackMetadata(
  previous: CodeDeliveryPullRequestSummary,
  next: CodeDeliveryPullRequestSummary,
): CodeDeliveryPullRequestSummary {
  const nextHasRegisteredStack =
    next.stack_number !== undefined || next.stack_size !== undefined;
  if (nextHasRegisteredStack) return next;

  const previousHasRegisteredStack =
    previous.stack_number !== undefined || previous.stack_size !== undefined;
  if (previousHasRegisteredStack) {
    return {
      ...next,
      ...(previous.stack_number !== undefined
        ? { stack_number: previous.stack_number }
        : {}),
      ...(previous.stack_size !== undefined
        ? { stack_size: previous.stack_size }
        : {}),
      ...(next.stack_parent_number !== undefined
        ? { stack_parent_number: next.stack_parent_number }
        : previous.stack_parent_number !== undefined
          ? { stack_parent_number: previous.stack_parent_number }
          : {}),
    };
  }

  const previousHasInferredStack =
    previous.stack_parent_number !== undefined ||
    previous.unregistered_stack_numbers !== undefined;
  if (!previousHasInferredStack) return next;

  return {
    ...next,
    ...(next.stack_parent_number === undefined &&
    previous.stack_parent_number !== undefined
      ? { stack_parent_number: previous.stack_parent_number }
      : {}),
    ...(next.unregistered_stack_numbers === undefined &&
    previous.unregistered_stack_numbers !== undefined
      ? { unregistered_stack_numbers: previous.unregistered_stack_numbers }
      : {}),
  };
}

/** True when the pull request belongs to a registered or inferred stack. */
export function isStackedPullRequest(
  item: CodeDeliveryPullRequestSummary,
): boolean {
  return (
    item.stack_parent_number !== undefined ||
    item.unregistered_stack_numbers !== undefined ||
    (item.stack_number !== undefined && (item.stack_size ?? 0) > 1)
  );
}

/** One delivery row placed in its stack lane. */
export type StackedRow = {
  /** The summary's own id, so a virtualized list can key on the row. */
  id: string;
  item: CodeDeliveryPullRequestSummary;
  /** Indent level: 0 for roots and orphans, one per hop otherwise. */
  depth: number;
  /**
   * Set on a child whose parent is not among the rows — filtered out, on a
   * later page, or lost to a cycle. The row stays at depth 0 and names the
   * parent instead of silently flattening.
   */
  stackedOn?: number;
};

/** Chains deeper than this render flat from there; nobody reads indent 11. */
const MAX_STACK_DEPTH = 10;

function rowKey(item: CodeDeliveryPullRequestSummary): string {
  const repo = item.repository;
  return `${repo.host.toLowerCase()}/${repo.owner.toLowerCase()}/${repo.name.toLowerCase()}#${item.number}`;
}

function parentKey(item: CodeDeliveryPullRequestSummary): string | null {
  if (item.stack_parent_number === undefined) return null;
  const repo = item.repository;
  return `${repo.host.toLowerCase()}/${repo.owner.toLowerCase()}/${repo.name.toLowerCase()}#${item.stack_parent_number}`;
}

/**
 * Order page rows into stack lanes: children directly follow their parent,
 * indented one level per hop, and roots keep the incoming order. The parent
 * pointer comes from the durable fact set server-side, so a child whose
 * parent is not on this page carries `stackedOn` rather than a hidden edge.
 * A cycle breaks at its first revisited node and the remainder render as
 * orphans, never dropped.
 */
export function arrangeStackLanes(
  items: readonly CodeDeliveryPullRequestSummary[],
): StackedRow[] {
  const present = new Map<string, CodeDeliveryPullRequestSummary>();
  for (const item of items) present.set(rowKey(item), item);

  const childrenOf = new Map<string, CodeDeliveryPullRequestSummary[]>();
  const roots: StackedRow[] = [];
  for (const item of items) {
    const parent = parentKey(item);
    if (parent !== null && present.has(parent)) {
      const siblings = childrenOf.get(parent) ?? [];
      siblings.push(item);
      childrenOf.set(parent, siblings);
    } else if (parent !== null) {
      roots.push({
        id: item.id,
        item,
        depth: 0,
        stackedOn: item.stack_parent_number,
      });
    } else {
      roots.push({ id: item.id, item, depth: 0 });
    }
  }

  const out: StackedRow[] = [];
  const emitted = new Set<string>();
  const emit = (row: StackedRow) => {
    const key = rowKey(row.item);
    if (emitted.has(key)) return;
    emitted.add(key);
    out.push(row);
    for (const child of childrenOf.get(key) ?? []) {
      emit({
        id: child.id,
        item: child,
        depth: Math.min(row.depth + 1, MAX_STACK_DEPTH),
      });
    }
  };
  for (const root of roots) emit(root);

  // Anything left sits on a cycle: no chain from it reaches a root. Break
  // the cycle in incoming order and surface each as an orphan.
  for (const item of items) {
    const key = rowKey(item);
    if (emitted.has(key)) continue;
    emit({ id: item.id, item, depth: 0, stackedOn: item.stack_parent_number });
  }
  return out;
}
