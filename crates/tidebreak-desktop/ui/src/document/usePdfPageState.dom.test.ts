// @vitest-environment jsdom
import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { usePdfPageState } from "./usePdfPageState";

afterEach(() => {
  cleanup();
  window.sessionStorage.clear();
});

describe("usePdfPageState", () => {
  it("returns to the page you left off on, per document", () => {
    const first = renderHook(() => usePdfPageState("doc-a", { numPages: 20 }));
    act(() => first.result.current.setCurrentPage(7));
    first.unmount();

    // A different source starts at its own beginning, not on page 7.
    const other = renderHook(() => usePdfPageState("doc-b", { numPages: 20 }));
    expect(other.result.current.currentPage).toBe(1);
    other.unmount();

    // Reopening the first one lands where it was closed.
    const reopened = renderHook(() => usePdfPageState("doc-a", { numPages: 20 }));
    expect(reopened.result.current.currentPage).toBe(7);
  });

  it("keeps the page inside the document once its length is known", () => {
    // A remembered page can outlive the document it was recorded against — a
    // shorter re-import, say — and must not leave the viewer on a page that is
    // not there.
    const { result } = renderHook(() => usePdfPageState("doc-a", { numPages: 20 }));
    act(() => result.current.setCurrentPage(15));

    const reopened = renderHook(() => usePdfPageState("doc-a", { numPages: 4 }));
    expect(reopened.result.current.currentPage).toBe(4);

    act(() => reopened.result.current.setCurrentPage(99));
    expect(reopened.result.current.currentPage).toBe(4);
  });

  it("honours a requested page once, then lets the reader move away", () => {
    const { result, rerender } = renderHook(
      ({ targetPage }: { targetPage?: number }) =>
        usePdfPageState("doc-a", { numPages: 20, targetPage }),
      { initialProps: { targetPage: 12 as number | undefined } },
    );
    expect(result.current.currentPage).toBe(12);

    act(() => result.current.setCurrentPage(13));
    rerender({ targetPage: 12 });
    expect(result.current.currentPage).toBe(13);
  });
});
