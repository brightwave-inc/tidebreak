import {
  useEffect,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from "react";

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/** Ignore shortcut keys until the card has been on screen this long. */
export const APPROVAL_SHORTCUT_GRACE_MS = 300;

export type ApprovalChoiceItem = {
  key: string;
  label: string;
  muted?: boolean;
  /** Expands hidden rungs instead of deciding. */
  expand?: boolean;
};

/**
 * Numbered consent rows with the keyboard contract ApprovalCard established:
 * highlight, a mount grace period, arrows, 1–9 jump, Enter, Submit, and a
 * mount focus rule that never steals an input, a textarea, or another list.
 */
export function ApprovalChoiceList({
  options,
  disabled,
  describedBy,
  headingRef,
  note,
  onChoose,
  onExpand,
}: {
  options: ApprovalChoiceItem[];
  disabled?: boolean;
  describedBy?: string;
  headingRef: RefObject<HTMLElement | null>;
  note?: ReactNode;
  onChoose: (index: number) => void;
  onExpand?: () => void;
}) {
  const [highlight, setHighlight] = useState(0);
  const rowRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const hasAutoFocused = useRef(false);
  const mountedAt = useRef(0);
  const optionsRef = useRef(options);
  const highlightRef = useRef(highlight);
  const disabledRef = useRef(disabled);
  const onChooseRef = useRef(onChoose);
  const onExpandRef = useRef(onExpand);
  optionsRef.current = options;
  highlightRef.current = highlight;
  disabledRef.current = disabled;
  onChooseRef.current = onChoose;
  onExpandRef.current = onExpand;
  const safeHighlight = Math.min(highlight, Math.max(options.length - 1, 0));

  useEffect(() => {
    if (hasAutoFocused.current || disabled) return;
    hasAutoFocused.current = true;
    mountedAt.current = Date.now();
    const active = document.activeElement;
    const focusedElsewhere =
      active instanceof HTMLElement &&
      (active.isContentEditable ||
        active.tagName === "INPUT" ||
        active.tagName === "TEXTAREA" ||
        active.closest('[aria-label="Approval choices"]') !== null);
    if (focusedElsewhere) return;
    headingRef.current?.focus({ preventScroll: true });
  }, [disabled, headingRef]);

  const shortcutsReady = () =>
    Date.now() - mountedAt.current >= APPROVAL_SHORTCUT_GRACE_MS;

  const activate = (index: number) => {
    const option = optionsRef.current[index];
    if (!option || disabledRef.current) return;
    if (option.expand) {
      onExpandRef.current?.();
      // The revealed grants take the indices "More options" occupied, so
      // leaving the highlight here would park it on a broader grant and let
      // the next Enter commit it — the one thing this list exists to prevent.
      setHighlight(0);
      rowRefs.current[0]?.focus();
      return;
    }
    setHighlight(index);
    onChooseRef.current(index);
  };

  const submitSelected = () => {
    const current = optionsRef.current;
    const index = Math.min(
      highlightRef.current,
      Math.max(current.length - 1, 0),
    );
    const option = current[index];
    if (!option || option.expand || disabledRef.current) return;
    onChooseRef.current(index);
  };

  const focusRow = (to: number) => {
    const current = optionsRef.current;
    if (current.length === 0) return;
    const wrapped = ((to % current.length) + current.length) % current.length;
    if (!current[wrapped]?.expand) setHighlight(wrapped);
    rowRefs.current[wrapped]?.focus();
  };

  useEffect(() => {
    const root = headingRef.current?.closest("section");
    if (!root) return;
    const onCardKeyDown = (event: KeyboardEvent) => {
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        target.closest("[data-approval-submit]")
      ) {
        return;
      }
      if (event.key === " ") {
        event.preventDefault();
        return;
      }
      if (!shortcutsReady()) {
        if (
          event.key === "Enter" ||
          event.key === "ArrowDown" ||
          event.key === "ArrowUp" ||
          /^[1-9]$/.test(event.key)
        ) {
          event.preventDefault();
        }
        return;
      }
      if (event.key === "Enter") {
        event.preventDefault();
        const from =
          target instanceof HTMLElement
            ? rowRefs.current.findIndex((row) => row === target)
            : -1;
        if (from >= 0 && optionsRef.current[from]?.expand) {
          activate(from);
          return;
        }
        submitSelected();
        return;
      }
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const from =
          target instanceof HTMLElement
            ? rowRefs.current.findIndex((row) => row === target)
            : -1;
        const index = from >= 0 ? from : highlightRef.current;
        focusRow(index + (event.key === "ArrowDown" ? 1 : -1));
        return;
      }
      if (/^[1-9]$/.test(event.key)) {
        const next = Number(event.key) - 1;
        if (next < optionsRef.current.length) {
          event.preventDefault();
          focusRow(next);
        }
      }
    };
    root.addEventListener("keydown", onCardKeyDown);
    return () => root.removeEventListener("keydown", onCardKeyDown);
  }, [headingRef]);

  return (
    <div className="flex flex-col gap-3">
      <div
        role="group"
        aria-label="Approval choices"
        className="flex flex-col gap-0.5"
      >
        {options.map((option, index) => (
          <button
            type="button"
            key={option.key}
            ref={(node) => {
              rowRefs.current[index] = node;
            }}
            aria-describedby={describedBy}
            disabled={disabled}
            onClick={() => activate(index)}
            onFocus={() => {
              if (!option.expand) setHighlight(index);
            }}
            className={cn(
              "focus-visible:ring-ring flex cursor-pointer items-baseline gap-2.5 rounded-md px-3 py-2.5 text-left text-sm outline-hidden focus-visible:ring-2",
              index === safeHighlight ? "bg-muted" : "hover:bg-muted/60",
              disabled && "opacity-60",
            )}
          >
            <span className="text-muted-foreground w-4 shrink-0 text-xs tabular-nums">
              {index + 1}.
            </span>
            <span
              className={cn(
                "flex-1 text-left",
                option.muted && "text-muted-foreground",
              )}
            >
              {option.label}
            </span>
          </button>
        ))}
      </div>
      {note}
      <div className="flex items-center justify-between gap-2 text-xs">
        <span className="text-muted-foreground">
          ↑↓ choose · 1–{Math.min(options.length, 9)} jump · click or Submit
          confirms
        </span>
        <Button
          size="sm"
          disabled={disabled}
          data-approval-submit=""
          onClick={submitSelected}
        >
          Submit
        </Button>
      </div>
    </div>
  );
}
