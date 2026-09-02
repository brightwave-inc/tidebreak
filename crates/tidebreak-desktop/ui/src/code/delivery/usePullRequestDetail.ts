import type {
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestSummary,
  CodeDeliveryPullRequestTarget,
} from "../../api/types";
import { pullRequestMatchesTarget } from "./helpers";
import { useEffect, useRef, useState } from "react";

export type TargetDetailState<T> = {
  key: string;
  pending: boolean;
  detail: T | null;
};

/** Keyboard walks wait this long before the detail pane loads the next row. */
export const PULL_REQUEST_DETAIL_SELECTION_DEBOUNCE_MS = 140;

/** Details kept warm so walking back up the list does not refetch. */
export const MAX_PULL_REQUEST_DETAIL_CACHE = 24;

/**
 * Which pull request the detail pane shows, and what it already knows about it.
 *
 * Selection has two sources. A route target (`?pr=` on a repository) picks
 * the row the page opened on; a click or an arrow key picks one by hand. The
 * hand-picked one wins: once the reader has chosen, a target detail that
 * lands later does not steal the pane back. `targetKey` changing resets that
 * fence along with the selection, because a new route is a new request.
 *
 * The cache is a small LRU keyed by pull request id. It seeds the pane with
 * a detail the pane fetched earlier, but only when that detail is at least as
 * fresh as the summary row now showing.
 */
export function usePullRequestDetail(targetKey: string | null) {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detailLoadDelayMs, setDetailLoadDelayMs] = useState(0);
  const [targetDetailState, setTargetDetailState] =
    useState<TargetDetailState<CodeDeliveryPullRequestDetail> | null>(null);
  const routeSelectionFenced = useRef(false);
  const detailCache = useRef(new Map<string, CodeDeliveryPullRequestDetail>());

  useEffect(() => {
    routeSelectionFenced.current = false;
    setSelectedId(null);
    setDetailLoadDelayMs(0);
    setTargetDetailState(
      targetKey ? { key: targetKey, pending: true, detail: null } : null,
    );
  }, [targetKey]);

  const selectItem = (id: string, debounceDetail = false) => {
    routeSelectionFenced.current = true;
    setDetailLoadDelayMs(
      debounceDetail ? PULL_REQUEST_DETAIL_SELECTION_DEBOUNCE_MS : 0,
    );
    setSelectedId(id);
  };

  const closeDetail = () => {
    routeSelectionFenced.current = true;
    setDetailLoadDelayMs(0);
    setSelectedId(null);
  };

  const rememberDetail = (detail: CodeDeliveryPullRequestDetail) => {
    detailCache.current.delete(detail.summary.id);
    detailCache.current.set(detail.summary.id, detail);
    if (detailCache.current.size > MAX_PULL_REQUEST_DETAIL_CACHE) {
      const oldest = detailCache.current.keys().next().value;
      if (oldest) detailCache.current.delete(oldest);
    }
  };

  /** A list query started fetching the route target's detail. */
  const beginTargetDetail = (key: string) => {
    setTargetDetailState((current) => ({
      key,
      pending: true,
      detail: current?.key === key ? current.detail : null,
    }));
  };

  /**
   * The route target's detail arrived. It becomes the selection unless the
   * reader already picked a row by hand.
   */
  const adoptTargetDetail = (
    key: string,
    detail: CodeDeliveryPullRequestDetail,
  ) => {
    rememberDetail(detail);
    setTargetDetailState({ key, pending: false, detail });
    if (!routeSelectionFenced.current) {
      setSelectedId(detail.summary.id);
    }
  };

  /** The route target's detail failed; the pane stops waiting for it. */
  const failTargetDetail = (key: string) => {
    setTargetDetailState((current) =>
      current?.key === key ? { ...current, pending: false } : current,
    );
  };

  /**
   * True while the selected row is the route target and its detail is still
   * on its way, so the pane can show a placeholder rather than a second fetch.
   */
  const pendingTargetDetail = (
    selected: CodeDeliveryPullRequestSummary | null,
    target: CodeDeliveryPullRequestTarget | undefined,
  ): boolean =>
    Boolean(
      selected &&
        target &&
        targetDetailState?.key === targetKey &&
        targetDetailState.pending &&
        !targetDetailState.detail &&
        pullRequestMatchesTarget(selected, target),
    );

  /** What the pane can render before its own fetch answers, if anything. */
  const initialDetail = (
    selected: CodeDeliveryPullRequestSummary | null,
  ): CodeDeliveryPullRequestDetail | undefined => {
    if (!selected) return undefined;
    if (targetDetailState?.detail?.summary.id === selected.id) {
      return targetDetailState.detail;
    }
    const cached = detailCache.current.get(selected.id);
    return cached && cached.summary.updated_at >= selected.updated_at
      ? cached
      : undefined;
  };

  return {
    selectedId,
    detailLoadDelayMs,
    selectItem,
    closeDetail,
    rememberDetail,
    beginTargetDetail,
    adoptTargetDetail,
    failTargetDetail,
    pendingTargetDetail,
    initialDetail,
  };
}
