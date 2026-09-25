import { formatDistanceToNowStrict } from "date-fns";
import { CircleAlert, MessageSquare, SquareTerminal } from "lucide-react";
import type { ReactNode } from "react";

import { CommandGroup, CommandItem } from "@/components/ui/command";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import type { MessageSearchHit } from "../generated/wire";
import {
  hitKey,
  hitSourceLabel,
  hitTitle,
  snippetSegments,
} from "./messageSearch";
import type { MessageSearchState } from "./useMessageSearch";

/**
 * The palette's Messages section: what was said in the person's chats and
 * code sessions that matches the query, newest first.
 *
 * Every state is a row in the section rather than a replacement for the
 * list, so the commands above it never move while a search is out. The arrow
 * keys walk the hits and pass over the status rows.
 */
export function MessageSearchSection({
  state,
  onSelect,
  now,
}: {
  state: MessageSearchState;
  onSelect: (hit: MessageSearchHit) => void;
  /** Fixed in stories so relative times do not drift. */
  now?: Date;
}) {
  if (state.status === "idle") return null;
  const pending = state.indexing?.pending_conversations ?? 0;
  const failed = state.indexing?.failed_conversations ?? 0;
  const searching = state.status === "loading";
  return (
    <CommandGroup
      heading={
        <span className="flex items-center gap-1.5">
          Messages
          {searching && state.hits.length > 0 && (
            <Spinner className="size-3" aria-label="Searching messages" />
          )}
        </span>
      }
      className="p-0"
    >
      {state.hits.map((hit) => (
        <MessageHitRow
          key={hitKey(hit)}
          hit={hit}
          now={now}
          onSelect={() => onSelect(hit)}
        />
      ))}
      {searching && state.hits.length === 0 && (
        <StatusRow standsIn icon={<Spinner className="size-3.5" aria-hidden />}>
          Searching messages…
        </StatusRow>
      )}
      {state.status === "error" && (
        <StatusRow
          standsIn
          icon={<CircleAlert className="size-3.5 text-critical" aria-hidden />}
        >
          {state.error ?? "Could not search messages."}
        </StatusRow>
      )}
      {state.status === "ready" && state.hits.length === 0 && (
        <StatusRow standsIn>No messages match “{state.query}”.</StatusRow>
      )}
      {pending > 0 && state.status !== "error" && (
        <StatusRow>
          Still indexing {pending}{" "}
          {pending === 1 ? "conversation" : "conversations"}, so older messages
          may be missing.
        </StatusRow>
      )}
      {failed > 0 && state.status !== "error" && (
        <StatusRow>
          {failed} {failed === 1 ? "conversation" : "conversations"} could not
          be indexed. Their older messages are not searchable.
        </StatusRow>
      )}
    </CommandGroup>
  );
}

/**
 * A line of status in the section, laid out like a hit so its words start
 * where the hits' titles do. The icon column is empty unless the status has
 * a mark of its own.
 *
 * Not a live region: the list is a listbox, which may only hold options and
 * groups. `messageSearchAnnouncement` says the same thing from a live region
 * outside it. A line that stands in for the hits (searching, failed, nothing
 * found) is a disabled option, so the listbox is never empty; the arrow keys
 * pass over it.
 */
function StatusRow({
  icon,
  standsIn = false,
  children,
}: {
  icon?: ReactNode;
  standsIn?: boolean;
  children: ReactNode;
}) {
  const body = (
    <>
      <span className="flex h-4 w-4 shrink-0 items-center justify-center">
        {icon}
      </span>
      <span className="min-w-0 flex-1 pt-px">{children}</span>
    </>
  );
  const layout =
    "flex items-start gap-2.5 px-2.5 py-2 text-xs text-muted-foreground";
  if (!standsIn) return <div className={layout}>{body}</div>;
  return (
    <CommandItem
      disabled
      value="message-search-status"
      // A disabled option keeps full ink: it is the answer, not an option
      // that is switched off.
      className={cn(layout, "cursor-default data-[disabled=true]:opacity-100")}
    >
      {body}
    </CommandItem>
  );
}

/** What a screen reader hears about the search, in a few words. */
export function messageSearchAnnouncement(state: MessageSearchState): string {
  switch (state.status) {
    case "idle":
      return "";
    case "loading":
      return "Searching messages";
    case "error":
      return state.error ?? "Could not search messages.";
    case "ready": {
      const count = state.hits.length;
      const found =
        count === 0
          ? "No messages match"
          : `${count} ${count === 1 ? "message matches" : "messages match"}`;
      const pending = state.indexing?.pending_conversations ?? 0;
      return pending > 0
        ? `${found}. Still indexing ${pending} ${pending === 1 ? "conversation" : "conversations"}.`
        : found;
    }
  }
}

/** One hit: where it was said, who said it, and the words around it. */
function MessageHitRow({
  hit,
  now,
  onSelect,
}: {
  hit: MessageSearchHit;
  now?: Date;
  onSelect: () => void;
}) {
  const Icon = hit.kind === "chat" ? MessageSquare : SquareTerminal;
  const title = hitTitle(hit);
  const kind = hit.kind === "chat" ? "Work" : "Code";
  const when = relativeWhen(hit.created_at, now);
  const segments = snippetSegments(hit.snippet, hit.ranges);
  return (
    <CommandItem
      value={hitKey(hit)}
      aria-label={`${title}, ${kind}${hit.archived ? ", archived" : ""}: ${hit.snippet}`}
      onSelect={onSelect}
      className="items-start gap-2.5 px-2.5 py-2"
    >
      <Icon className="mt-0.5 text-muted-foreground" aria-hidden />
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex min-w-0 items-baseline gap-2">
          <span className="min-w-0 truncate text-sm font-medium">{title}</span>
          <span className="shrink-0 text-xs text-muted-foreground">
            {kind}
            {hit.archived && " · Archived"}
          </span>
        </span>
        <span
          className={cn(
            "line-clamp-2 text-xs text-muted-foreground break-words [overflow-wrap:anywhere]",
            // What a tool call acted on is a command, a path, or a query:
            // the machine's voice.
            hit.source === "tool" && "font-mono",
          )}
        >
          <span className="sr-only">{hitSourceLabel(hit)}: </span>
          {/* Runs of one snippet never reorder, so their place is their key. */}
          {segments.map((segment, index) =>
            segment.match ? (
              <mark key={index} className="search-match">
                {segment.text}
              </mark>
            ) : (
              <span key={index}>{segment.text}</span>
            ),
          )}
        </span>
      </span>
      {when && (
        <span className="shrink-0 pt-0.5 text-xs text-muted-foreground">
          {when}
        </span>
      )}
    </CommandItem>
  );
}

/** "3 days ago", or nothing when the time does not parse. */
function relativeWhen(value: string, now?: Date): string {
  const at = new Date(value);
  if (Number.isNaN(at.getTime())) return "";
  if (now) {
    const seconds = Math.max(0, (now.getTime() - at.getTime()) / 1000);
    return compactAge(seconds);
  }
  try {
    return formatDistanceToNowStrict(at, { addSuffix: true });
  } catch {
    return "";
  }
}

/** The same words `formatDistanceToNowStrict` uses, against a fixed now. */
function compactAge(seconds: number): string {
  const units: [number, string][] = [
    [365 * 86_400, "year"],
    [30 * 86_400, "month"],
    [86_400, "day"],
    [3_600, "hour"],
    [60, "minute"],
  ];
  for (const [size, unit] of units) {
    const count = Math.floor(seconds / size);
    if (count >= 1) return `${count} ${unit}${count === 1 ? "" : "s"} ago`;
  }
  return "just now";
}
