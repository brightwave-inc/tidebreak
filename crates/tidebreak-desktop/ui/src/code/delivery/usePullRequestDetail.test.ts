// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import type {
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestSummary,
} from "../../api/types";
import { deliveryPullRequests } from "../../stories/fixtures";
import { codeDeliveryRepositoryTarget } from "../CodeDeliveryStore";
import {
  MAX_PULL_REQUEST_DETAIL_CACHE,
  PULL_REQUEST_DETAIL_SELECTION_DEBOUNCE_MS,
  usePullRequestDetail,
} from "./usePullRequestDetail";

const first = deliveryPullRequests[0] as CodeDeliveryPullRequestSummary;
const target = {
  repository: codeDeliveryRepositoryTarget(first.repository),
  number: first.number,
};
const targetKey = "github.com/brightwave-inc/tidebreak:pull-request:2251";

function detailOf(
  summary: CodeDeliveryPullRequestSummary,
): CodeDeliveryPullRequestDetail {
  return { summary } as unknown as CodeDeliveryPullRequestDetail;
}

function summaryWith(
  id: string,
  updated_at: string,
): CodeDeliveryPullRequestSummary {
  return { ...first, id, updated_at };
}

afterEach(cleanup);

describe("usePullRequestDetail", () => {
  it("selects by hand with a debounce for walks and none for clicks", () => {
    const { result } = renderHook(() => usePullRequestDetail(null));
    act(() => result.current.selectItem("pr-1", true));
    expect(result.current.selectedId).toBe("pr-1");
    expect(result.current.detailLoadDelayMs).toBe(
      PULL_REQUEST_DETAIL_SELECTION_DEBOUNCE_MS,
    );
    act(() => result.current.selectItem("pr-2"));
    expect(result.current.detailLoadDelayMs).toBe(0);
    act(() => result.current.closeDetail());
    expect(result.current.selectedId).toBeNull();
  });

  it("lets the route target pick the row until the reader picks one by hand", () => {
    const { result } = renderHook(() => usePullRequestDetail(targetKey));
    expect(result.current.pendingTargetDetail(first, target)).toBe(true);

    act(() => result.current.adoptTargetDetail(targetKey, detailOf(first)));
    expect(result.current.selectedId).toBe(first.id);
    expect(result.current.pendingTargetDetail(first, target)).toBe(false);
    expect(result.current.initialDetail(first)?.summary.id).toBe(first.id);

    const other = summaryWith("pr-other", first.updated_at);
    act(() => result.current.selectItem(other.id));
    act(() =>
      result.current.adoptTargetDetail(
        targetKey,
        detailOf(summaryWith(first.id, "2026-09-01T00:00:00Z")),
      ),
    );
    expect(result.current.selectedId).toBe(other.id);
  });

  it("resets the selection and the fence when the route target changes", () => {
    const { result, rerender } = renderHook(
      ({ key }: { key: string | null }) => usePullRequestDetail(key),
      { initialProps: { key: targetKey as string | null } },
    );
    act(() => result.current.selectItem("pr-by-hand"));
    rerender({ key: `${targetKey}-next` });
    expect(result.current.selectedId).toBeNull();
    act(() =>
      result.current.adoptTargetDetail(`${targetKey}-next`, detailOf(first)),
    );
    expect(result.current.selectedId).toBe(first.id);

    rerender({ key: null });
    expect(result.current.pendingTargetDetail(first, target)).toBe(false);
  });

  it("keeps a failed target from pending forever", () => {
    const { result } = renderHook(() => usePullRequestDetail(targetKey));
    act(() => result.current.beginTargetDetail(targetKey));
    act(() => result.current.failTargetDetail(targetKey));
    act(() => result.current.selectItem(first.id));
    expect(result.current.pendingTargetDetail(first, target)).toBe(false);
  });

  it("seeds the pane from the cache only while the cached detail is as fresh as the row", () => {
    const { result } = renderHook(() => usePullRequestDetail(null));
    const stale = summaryWith("pr-1", "2026-08-01T00:00:00Z");
    act(() => result.current.rememberDetail(detailOf(stale)));
    expect(result.current.initialDetail(stale)?.summary.id).toBe("pr-1");
    expect(
      result.current.initialDetail(summaryWith("pr-1", "2026-08-02T00:00:00Z")),
    ).toBeUndefined();
    expect(result.current.initialDetail(null)).toBeUndefined();
  });

  it("evicts the least recently remembered detail past the cache size", () => {
    const { result } = renderHook(() => usePullRequestDetail(null));
    const at = "2026-08-01T00:00:00Z";
    act(() => {
      for (let index = 0; index < MAX_PULL_REQUEST_DETAIL_CACHE; index += 1) {
        result.current.rememberDetail(detailOf(summaryWith(`pr-${index}`, at)));
      }
      // Touching the oldest moves it to the back of the queue.
      result.current.rememberDetail(detailOf(summaryWith("pr-0", at)));
      result.current.rememberDetail(detailOf(summaryWith("pr-extra", at)));
    });
    expect(result.current.initialDetail(summaryWith("pr-0", at))).toBeDefined();
    expect(
      result.current.initialDetail(summaryWith("pr-1", at)),
    ).toBeUndefined();
    expect(
      result.current.initialDetail(summaryWith("pr-extra", at)),
    ).toBeDefined();
  });
});
