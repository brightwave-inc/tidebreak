import { isEditableKeyboardTarget } from "./helpers";
import { useEffect, useRef } from "react";

/**
 * Arrow keys walk the open detail through the list; Escape closes it.
 *
 * The listener exists only while a row is selected, so the keys mean nothing
 * on a bare list. Walking off the bottom asks for the next page rather than
 * wrapping; walking off the top does nothing. Keys with a modifier, keys the
 * page already handled, and keys typed into a field are all left alone.
 */
export function usePullRequestKeyboardNav({
  selectedId,
  displayIds,
  nextCursor,
  loadingMore,
  onSelect,
  onClose,
  onLoadMore,
}: {
  selectedId: string | null;
  displayIds: readonly string[];
  nextCursor: string | undefined;
  loadingMore: boolean;
  /** Select a neighbouring row; the detail load is debounced for a walk. */
  onSelect: (id: string) => void;
  onClose: () => void;
  onLoadMore: (cursor: string) => void;
}) {
  const handlers = useRef({ onSelect, onClose, onLoadMore });
  handlers.current = { onSelect, onClose, onLoadMore };

  useEffect(() => {
    if (!selectedId) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (
        event.defaultPrevented ||
        event.altKey ||
        event.metaKey ||
        event.ctrlKey
      ) {
        return;
      }
      if (isEditableKeyboardTarget(event.target)) return;
      if (event.key === "Escape") {
        event.preventDefault();
        handlers.current.onClose();
        return;
      }
      if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
      const index = displayIds.indexOf(selectedId);
      if (index < 0) return;
      const nextIndex = event.key === "ArrowDown" ? index + 1 : index - 1;
      if (nextIndex < 0) return;
      event.preventDefault();
      if (nextIndex >= displayIds.length) {
        if (nextCursor && !loadingMore) handlers.current.onLoadMore(nextCursor);
        return;
      }
      const nextId = displayIds[nextIndex];
      if (nextId) handlers.current.onSelect(nextId);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [displayIds, loadingMore, nextCursor, selectedId]);
}
