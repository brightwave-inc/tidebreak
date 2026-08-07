import { useCallback, useEffect, useRef, useState } from "react";
import { ListTree, MessageSquare } from "lucide-react";

import type { ChatMessage } from "./MessageList";
import { toolCallPresentation } from "./ToolCallCard";
import { ToolIcon } from "./ToolIcon";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

const RAIL_STORAGE_KEY = "openwave.transcript-rail-visible";
const INDICATOR_SIZE = 24;
const PROXIMITY_THRESHOLD = 400;

export type RailIndicatorCandidate = {
  anchorId: string;
  desiredTop: number;
  distanceFromViewport: number;
};

export type TranscriptNavigationEntry = {
  anchorId: string;
  kind: "user" | "tool";
  label: string;
  toolName?: string;
  active: boolean;
};

/** Build a local table of contents from the transcript already on screen. */
export function transcriptNavigationEntries(
  messages: readonly ChatMessage[],
): TranscriptNavigationEntry[] {
  const entries: TranscriptNavigationEntry[] = [];
  for (const message of messages) {
    if (message.role === "user") {
      entries.push({
        anchorId: message.id,
        kind: "user",
        label: compactLabel(message.text, "User message"),
        active: false,
      });
      continue;
    }
    if (message.role !== "tool") continue;
    const presentation = toolCallPresentation(message.name, message.status);
    entries.push({
      anchorId: message.id,
      kind: "tool",
      label: presentation.title,
      toolName: message.name,
      active:
        presentation.tone === "running" ||
        presentation.tone === "waiting_approval",
    });
  }
  return entries;
}

function compactLabel(value: string, fallback: string): string {
  const compact = value.replace(/\s+/g, " ").trim();
  if (!compact) return fallback;
  return compact.length <= 72 ? compact : `${compact.slice(0, 71).trimEnd()}…`;
}

export function TranscriptNavigation({
  entries,
  scrollElement,
  activeAnchor,
  onJump,
}: {
  entries: readonly TranscriptNavigationEntry[];
  scrollElement: HTMLDivElement | null;
  activeAnchor?: string;
  onJump: (anchorId: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [railVisible, setRailVisible] = useStoredRailVisibility();
  const indicatorRefs = useRailPositions(scrollElement, entries, railVisible);
  const setIndicatorRef = useCallback(
    (anchorId: string, node: HTMLButtonElement | null) => {
      if (node) indicatorRefs.current.set(anchorId, node);
      else indicatorRefs.current.delete(anchorId);
    },
    [indicatorRefs],
  );

  if (entries.length === 0) return null;

  return (
    <>
      {railVisible && (
        <div
          className="pointer-events-none absolute top-0 right-2 bottom-0 z-[2] hidden w-6 p-2 md:block"
          aria-label="Transcript rail"
        >
          {entries.map((entry) => (
            <WithTooltip key={entry.anchorId} label={entry.label} side="left">
              <button
                ref={(node) => setIndicatorRef(entry.anchorId, node)}
                type="button"
                className={cn(
                  "text-muted-foreground hover:text-foreground hover:bg-muted pointer-events-auto absolute right-0 flex size-6 items-center justify-center rounded-full",
                  activeAnchor === entry.anchorId &&
                    "bg-accent text-accent-foreground",
                )}
                aria-label={entry.label}
                onClick={() => onJump(entry.anchorId)}
              >
                <EntryIcon entry={entry} />
              </button>
            </WithTooltip>
          ))}
        </div>
      )}

      <div className="absolute top-2 right-10 z-[3]">
        <Popover open={open} onOpenChange={setOpen}>
          <WithTooltip label="Transcript contents" side="left">
            <PopoverTrigger asChild>
              <Button
                type="button"
                variant="outline"
                size="icon-8"
                className="bg-background shadow-sm"
                aria-label="Transcript contents"
              >
                <ListTree size={16} />
              </Button>
            </PopoverTrigger>
          </WithTooltip>
          <PopoverContent align="end" sideOffset={6} className="w-72 p-0">
            <label className="flex cursor-pointer items-center gap-2 border-b px-3 py-2.5 text-sm">
              <Checkbox
                checked={railVisible}
                onCheckedChange={(checked) => setRailVisible(checked === true)}
              />
              Show rail indicators
            </label>
            <div className="max-h-80 overflow-y-auto p-1">
              {entries.map((entry) => (
                <button
                  key={entry.anchorId}
                  type="button"
                  className={cn(
                    "hover:bg-muted flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm",
                    activeAnchor === entry.anchorId && "bg-accent",
                  )}
                  onClick={() => {
                    onJump(entry.anchorId);
                    setOpen(false);
                  }}
                >
                  <EntryIcon entry={entry} />
                  <span className="truncate">{entry.label}</span>
                  {entry.active && (
                    <span
                      className="bg-primary ml-auto size-2 shrink-0 rounded-full"
                      aria-label="Active"
                    />
                  )}
                </button>
              ))}
            </div>
          </PopoverContent>
        </Popover>
      </div>
    </>
  );
}

function EntryIcon({ entry }: { entry: TranscriptNavigationEntry }) {
  return entry.kind === "user" ? (
    <MessageSquare className="text-muted-foreground size-3.5 shrink-0" />
  ) : (
    <ToolIcon
      name={entry.toolName ?? "other"}
      className="text-muted-foreground size-3.5 shrink-0"
    />
  );
}

function useStoredRailVisibility(): [boolean, (visible: boolean) => void] {
  const [visible, setVisible] = useState(() => {
    try {
      return window.localStorage.getItem(RAIL_STORAGE_KEY) !== "false";
    } catch {
      return true;
    }
  });
  const update = useCallback((next: boolean) => {
    setVisible(next);
    try {
      window.localStorage.setItem(RAIL_STORAGE_KEY, String(next));
    } catch {
      // A locked-down webview can reject storage; the in-memory choice remains.
    }
  }, []);
  return [visible, update];
}

/** Position indicators beside their rendered transcript anchors without rerendering. */
function useRailPositions(
  scrollElement: HTMLDivElement | null,
  entries: readonly TranscriptNavigationEntry[],
  visible: boolean,
) {
  const refs = useRef<Map<string, HTMLButtonElement>>(new Map());
  const frameRef = useRef<number | null>(null);

  useEffect(() => {
    if (!scrollElement || entries.length === 0 || !visible) return;

    const compute = () => {
      const container = scrollElement.getBoundingClientRect();
      const anchorRects = new Map<string, DOMRect>();
      for (const node of scrollElement.querySelectorAll<HTMLElement>(
        "[data-transcript-anchor]",
      )) {
        const id = node.dataset.transcriptAnchor;
        if (id && !anchorRects.has(id)) {
          anchorRects.set(id, node.getBoundingClientRect());
        }
      }

      const candidates: RailIndicatorCandidate[] = [];
      for (const entry of entries) {
        const indicator = refs.current.get(entry.anchorId);
        if (!indicator) continue;
        const rect = anchorRects.get(entry.anchorId);
        if (!rect) {
          indicator.style.display = "none";
          continue;
        }
        const relativeTop = rect.top - container.top;
        if (
          relativeTop < -PROXIMITY_THRESHOLD ||
          relativeTop > container.height + PROXIMITY_THRESHOLD
        ) {
          indicator.style.display = "none";
          continue;
        }

        candidates.push({
          anchorId: entry.anchorId,
          desiredTop: Math.max(
            0,
            Math.min(relativeTop, container.height - INDICATOR_SIZE),
          ),
          distanceFromViewport:
            relativeTop < 0
              ? -relativeTop
              : relativeTop > container.height
                ? relativeTop - container.height
                : 0,
        });
        const opacity =
          relativeTop < 0
            ? 1 - Math.abs(relativeTop) / PROXIMITY_THRESHOLD
            : relativeTop > container.height
              ? 1 -
                (relativeTop - container.height) / PROXIMITY_THRESHOLD
              : 1;
        indicator.style.display = "";
        indicator.style.opacity = String(Math.max(0, opacity));
      }

      const tops = layoutRailIndicatorTops(candidates, container.height);
      for (const candidate of candidates) {
        const indicator = refs.current.get(candidate.anchorId);
        if (!indicator) continue;
        const top = tops.get(candidate.anchorId);
        if (top === undefined) {
          indicator.style.display = "none";
          continue;
        }
        indicator.style.transform = `translateY(${top}px)`;
      }
    };

    const schedule = () => {
      if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
      frameRef.current = requestAnimationFrame(compute);
    };
    scrollElement.addEventListener("scroll", schedule, { passive: true });
    const observer = new ResizeObserver(schedule);
    observer.observe(scrollElement);
    const content = scrollElement.firstElementChild;
    if (content) observer.observe(content);
    schedule();

    return () => {
      scrollElement.removeEventListener("scroll", schedule);
      observer.disconnect();
      if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
    };
  }, [entries, scrollElement, visible]);

  return refs;
}

/**
 * Keep fixed-size transcript indicators from landing on top of one another.
 *
 * Several offscreen anchors clamp to the same top or bottom edge, and dense
 * activity can put nearby onscreen anchors less than one icon apart. Prefer
 * the entries nearest the viewport when there is not room for every button,
 * then push the retained positions apart while staying inside the rail.
 */
export function layoutRailIndicatorTops(
  candidates: readonly RailIndicatorCandidate[],
  containerHeight: number,
): Map<string, number> {
  const capacity = Math.max(0, Math.floor(containerHeight / INDICATOR_SIZE));
  if (capacity === 0 || candidates.length === 0) return new Map();

  const retained = [...candidates]
    .sort(
      (left, right) =>
        left.distanceFromViewport - right.distanceFromViewport ||
        left.desiredTop - right.desiredTop,
    )
    .slice(0, capacity)
    .sort((left, right) => left.desiredTop - right.desiredTop);
  const maxTop = containerHeight - INDICATOR_SIZE;
  const tops: number[] = [];
  for (const candidate of retained) {
    const previous = tops.at(-1);
    tops.push(
      previous === undefined
        ? candidate.desiredTop
        : Math.max(candidate.desiredTop, previous + INDICATOR_SIZE),
    );
  }

  // The forward pass can push a bottom cluster below the rail. Pull it back
  // from the end; because retained.length <= capacity, this cannot cross zero.
  tops[tops.length - 1] = Math.min(tops[tops.length - 1]!, maxTop);
  for (let index = tops.length - 2; index >= 0; index -= 1) {
    tops[index] = Math.min(tops[index]!, tops[index + 1]! - INDICATOR_SIZE);
  }

  return new Map(
    retained.map((candidate, index) => [candidate.anchorId, tops[index]!] as const),
  );
}
