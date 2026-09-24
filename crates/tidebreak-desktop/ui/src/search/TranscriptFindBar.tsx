import { ChevronDown, ChevronUp, Search, X } from "lucide-react";
import { forwardRef, useId, type KeyboardEvent } from "react";

import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { findCountLabel, type TranscriptFindState } from "./useTranscriptFind";

/**
 * Find in the open conversation: a field, how many messages match, and the
 * two steps through them.
 *
 * It floats over the transcript's top edge, the way a browser's find bar
 * does, so opening it moves nothing the reader was looking at. Enter steps
 * up the conversation to the next older match and Shift+Enter back down;
 * Escape closes it and hands focus back.
 */
export const TranscriptFindBar = forwardRef<
  HTMLInputElement,
  {
    query: string;
    onQueryChange: (query: string) => void;
    state: TranscriptFindState;
    onOlder: () => void;
    onNewer: () => void;
    onClose: () => void;
    className?: string;
  }
>(function TranscriptFindBar(
  { query, onQueryChange, state, onOlder, onNewer, onClose, className },
  ref,
) {
  const count = state.matches.length;
  const canStep = state.status === "ready" && count > 0;
  const label = findCountLabel(state);
  const pending = (state.indexing?.pending_conversations ?? 0) > 0;
  const failed = (state.indexing?.failed_conversations ?? 0) > 0;
  const countId = useId();

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "Enter") {
      event.preventDefault();
      if (event.shiftKey) onNewer();
      else onOlder();
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onClose();
    }
  }

  return (
    <div
      role="search"
      aria-label="Find in this conversation"
      className={cn(
        "flex w-[min(100%,26rem)] flex-col gap-1 rounded-lg border bg-popover px-2 py-1.5 text-popover-foreground shadow-sm focus-within:border-ring",
        className,
      )}
    >
      <div className="flex items-center gap-1">
        <Search
          className="mx-1 size-3.5 shrink-0 text-muted-foreground"
          aria-hidden
        />
        <input
          ref={ref}
          type="text"
          value={query}
          onChange={(event) => onQueryChange(event.target.value)}
          onKeyDown={onKeyDown}
          placeholder="Find in conversation"
          aria-label="Find in conversation"
          aria-describedby={countId}
          autoComplete="off"
          spellCheck={false}
          className="h-control-sm min-w-0 flex-1 bg-transparent px-1 text-sm outline-none placeholder:text-muted-foreground"
        />
        <span
          id={countId}
          aria-live="polite"
          className="flex shrink-0 items-center justify-end gap-1 px-1 text-xs text-muted-foreground tabular-nums"
        >
          {state.status === "loading" && (
            <Spinner className="size-3" aria-hidden />
          )}
          {label}
        </span>
        <WithTooltip label="Older match (Enter)">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label="Older match"
            disabled={!canStep}
            onClick={onOlder}
          >
            <ChevronUp aria-hidden />
          </Button>
        </WithTooltip>
        <WithTooltip label="Newer match (Shift+Enter)">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label="Newer match"
            disabled={!canStep}
            onClick={onNewer}
          >
            <ChevronDown aria-hidden />
          </Button>
        </WithTooltip>
        <WithTooltip label="Close (Esc)">
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            aria-label="Close find"
            onClick={onClose}
          >
            <X aria-hidden />
          </Button>
        </WithTooltip>
      </div>
      {state.status === "ready" && (pending || failed) && (
        <p className="px-1 pb-0.5 text-xs text-muted-foreground">
          {pending
            ? "This conversation is still being indexed, so older matches may be missing."
            : "Older messages in this conversation could not be indexed and are not searchable."}
        </p>
      )}
      {state.status === "error" && state.error && (
        <p role="alert" className="px-1 pb-0.5 text-xs text-critical">
          {state.error}
        </p>
      )}
    </div>
  );
});
