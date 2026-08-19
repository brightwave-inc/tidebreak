import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { create } from "zustand";

import { Button } from "@/components/ui/button";

export const FIRST_TASK_WALKTHROUGH_KEY =
  "tidebreak.first-task-walkthrough.v2";

export type FirstTaskWalkthroughOutcome = "completed" | "skipped";

/** Composer surface the walkthrough opens so the spotlight has something real to show. */
export type FirstTaskSurface = "model" | "tools" | "permissions" | null;

type WalkthroughStep = {
  id: string;
  surface: FirstTaskSurface;
  /** Opened menu content, preferred once the surface has mounted. */
  target: string;
  /** Trigger still in the bar, used before the menu portal exists. */
  fallbackTarget: string;
  title: string;
  body: string;
};

const STEPS: readonly WalkthroughStep[] = [
  {
    id: "model",
    surface: "model",
    target: "model-menu",
    fallbackTarget: "model",
    title: "Choose a model",
    body: "The model menu is open. Models differ in speed, reasoning, and the inputs they understand. The current default is a good place to start.",
  },
  {
    id: "internet",
    surface: "tools",
    target: "tools-menu",
    fallbackTarget: "tools",
    title: "Set internet access",
    body: "The Tools menu is open. Network is this chat's internet setting. Internet access is what web search and current information need. Leave it off when Tidebreak should use only your message and attachments.",
  },
  {
    id: "permissions",
    surface: "permissions",
    target: "permissions-menu",
    fallbackTarget: "permissions",
    title: "Choose a permission level",
    body: "The permission menu is open. Plan stays read-only, Ask confirms actions, Auto handles routine workspace work, and Allow all runs without asking in this chat. Ask is a balanced place to start.",
  },
  {
    id: "attachments",
    surface: "tools",
    target: "tools-menu",
    fallbackTarget: "tools",
    title: "Add attachments",
    body: "Attach files or a folder from this menu when the task needs your documents. Desktop Tidebreak can read what you attach.",
  },
  {
    id: "starters",
    surface: null,
    target: "starters",
    fallbackTarget: "starters",
    title: "Start a real task",
    body: "These prompts are complete. Pick one and send it to watch Tidebreak search, write, or build. You do not need to attach anything first.",
  },
] as const;

type ElementRect = {
  top: number;
  left: number;
  right: number;
  bottom: number;
  width: number;
  height: number;
};

type FirstTaskGuideState = {
  surface: FirstTaskSurface;
  setSurface: (surface: FirstTaskSurface) => void;
};

export const useFirstTaskGuide = create<FirstTaskGuideState>((set) => ({
  surface: null,
  setSurface: (surface) => set({ surface }),
}));

/**
 * Controlled open state for a composer menu the walkthrough is allowed to
 * force open. While that surface is active the menu stays open and is not
 * modal, so the walkthrough card can keep keyboard focus.
 */
export function useGuidedMenu(surface: Exclude<FirstTaskSurface, null>) {
  const guided = useFirstTaskGuide((state) => state.surface === surface);
  const [open, setOpen] = useState(false);
  return {
    guided,
    open: guided || open,
    modal: !guided,
    onOpenChange: (next: boolean) => {
      if (guided) return;
      setOpen(next);
    },
    onEscapeKeyDown: (event: { preventDefault: () => void }) => {
      if (guided) event.preventDefault();
    },
  };
}

export function shouldOfferFirstTaskWalkthrough(): boolean {
  try {
    const value = window.localStorage.getItem(FIRST_TASK_WALKTHROUGH_KEY);
    return value !== "completed" && value !== "skipped";
  } catch {
    return true;
  }
}

function storeOutcome(outcome: FirstTaskWalkthroughOutcome): void {
  try {
    window.localStorage.setItem(FIRST_TASK_WALKTHROUGH_KEY, outcome);
  } catch {
    // The walkthrough still closes when preference persistence is unavailable.
  }
}

function targetSelector(target: string): string {
  return `[data-first-task-target="${CSS.escape(target)}"]`;
}

function resolveTarget(step: WalkthroughStep): HTMLElement | null {
  return (
    document.querySelector<HTMLElement>(targetSelector(step.target)) ??
    document.querySelector<HTMLElement>(targetSelector(step.fallbackTarget))
  );
}

function focusComposer(): void {
  window.requestAnimationFrame(() => {
    document
      .querySelector<HTMLTextAreaElement>("[data-composer-input]")
      ?.focus();
  });
}

function focusableIn(root: HTMLElement): HTMLElement[] {
  return [
    ...root.querySelectorAll<HTMLElement>(
      'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
    ),
  ];
}

function clipPathFor(rect: ElementRect, inset: number): string {
  const left = rect.left - inset;
  const top = rect.top - inset;
  const right = rect.left + rect.width + inset;
  const bottom = rect.top + rect.height + inset;
  return `polygon(
    0% 0%, 0% 100%, 100% 100%, 100% 0%,
    0% 0%,
    ${left}px ${top}px,
    ${left}px ${bottom}px,
    ${right}px ${bottom}px,
    ${right}px ${top}px,
    ${left}px ${top}px,
    0% 0%
  )`;
}

export function FirstTaskWalkthrough({
  open,
  onClose,
}: {
  open: boolean;
  onClose: (outcome: FirstTaskWalkthroughOutcome) => void;
}) {
  const [stepIndex, setStepIndex] = useState(0);
  const [rect, setRect] = useState<ElementRect | null>(null);
  const cardRef = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  const setSurface = useFirstTaskGuide((state) => state.setSurface);
  const step = STEPS[stepIndex];

  useEffect(() => {
    if (!open) {
      setStepIndex(0);
      setRect(null);
      setSurface(null);
      return;
    }
    setSurface(step.surface);
  }, [open, setSurface, step.surface]);

  useEffect(() => {
    if (!open) return;

    let cancelled = false;
    let frames = 0;
    let target: HTMLElement | null = null;
    const observer = new ResizeObserver(() => {
      if (!target || cancelled) return;
      const next = target.getBoundingClientRect();
      setRect({
        top: next.top,
        left: next.left,
        right: next.right,
        bottom: next.bottom,
        width: next.width,
        height: next.height,
      });
    });

    const measure = (element: HTMLElement) => {
      const next = element.getBoundingClientRect();
      setRect({
        top: next.top,
        left: next.left,
        right: next.right,
        bottom: next.bottom,
        width: next.width,
        height: next.height,
      });
    };

    const find = () => {
      if (cancelled) return;
      const next = resolveTarget(step);
      if (!next) {
        setRect(null);
        if (frames++ < 30) {
          window.requestAnimationFrame(find);
        }
        return;
      }
      target = next;
      next.scrollIntoView({ block: "nearest", inline: "nearest" });
      measure(next);
      observer.observe(next);
    };
    find();

    const onViewport = () => {
      if (target) measure(target);
    };
    window.addEventListener("resize", onViewport);
    window.addEventListener("scroll", onViewport, true);
    return () => {
      cancelled = true;
      observer.disconnect();
      window.removeEventListener("resize", onViewport);
      window.removeEventListener("scroll", onViewport, true);
    };
  }, [open, step]);

  useEffect(() => {
    if (!open) return;
    cardRef.current?.focus();
  }, [open, stepIndex]);

  useEffect(() => {
    if (!open) return;
    const cycleFocus = (event: KeyboardEvent) => {
      const card = cardRef.current;
      if (!card) return;
      const focusable = focusableIn(card);
      if (focusable.length === 0) {
        event.preventDefault();
        card.focus();
        return;
      }
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      const active = document.activeElement;
      if (event.shiftKey) {
        if (active === first || !card.contains(active)) {
          event.preventDefault();
          last.focus();
        }
        return;
      }
      if (active === last || !card.contains(active)) {
        event.preventDefault();
        first.focus();
      }
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        storeOutcome("skipped");
        setSurface(null);
        onCloseRef.current("skipped");
        focusComposer();
        return;
      }
      if (event.key === "Tab") cycleFocus(event);
    };
    const onFocusIn = (event: FocusEvent) => {
      const card = cardRef.current;
      if (!card) return;
      const target = event.target;
      if (target instanceof Node && card.contains(target)) return;
      const focusable = focusableIn(card);
      (focusable[0] ?? card).focus();
    };
    window.addEventListener("keydown", onKeyDown, true);
    document.addEventListener("focusin", onFocusIn);
    return () => {
      window.removeEventListener("keydown", onKeyDown, true);
      document.removeEventListener("focusin", onFocusIn);
    };
  }, [open, setSurface]);

  function finish(outcome: FirstTaskWalkthroughOutcome) {
    storeOutcome(outcome);
    setSurface(null);
    onClose(outcome);
    focusComposer();
  }

  if (!open || typeof document === "undefined") return null;

  const dialogWidth = Math.min(320, window.innerWidth - 24);
  const above = rect !== null && rect.top >= 220;
  const dialogLeft = rect
    ? Math.max(
        12,
        Math.min(
          rect.left + rect.width / 2 - dialogWidth / 2,
          window.innerWidth - dialogWidth - 12,
        ),
      )
    : Math.max(12, (window.innerWidth - dialogWidth) / 2);
  const dialogTop = rect
    ? above
      ? rect.top - 12
      : rect.bottom + 12
    : window.innerHeight / 2;
  const inset = 4;

  return createPortal(
    <>
      {rect && (
        <div className="pointer-events-none fixed inset-0 z-[100]" aria-hidden="true">
          <div
            className="pointer-events-auto absolute inset-0 bg-black/50"
            style={{ clipPath: clipPathFor(rect, inset) }}
          />
          <div
            className="absolute rounded-[10px] ring-2 ring-[var(--brightwave)] ring-offset-2 ring-offset-transparent"
            style={{
              top: rect.top - inset,
              left: rect.left - inset,
              width: rect.width + inset * 2,
              height: rect.height + inset * 2,
            }}
          />
        </div>
      )}
      <div
        ref={cardRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="first-task-walkthrough-title"
        aria-describedby="first-task-walkthrough-body"
        tabIndex={-1}
        className="fixed z-[102] max-w-none rounded-xl border border-border bg-popover p-4 text-popover-foreground shadow-xl outline-none"
        style={{
          top: dialogTop,
          left: dialogLeft,
          width: dialogWidth,
          transform: rect
            ? above
              ? "translateY(-100%)"
              : undefined
            : "translateY(-50%)",
        }}
      >
        <div aria-live="polite" aria-atomic="true">
          <div className="flex items-center justify-between gap-3 text-xs text-muted-foreground">
            <span>Set up your first task</span>
            <span>
              {stepIndex + 1} of {STEPS.length}
            </span>
          </div>
          <h2
            id="first-task-walkthrough-title"
            className="mt-3 text-base font-semibold tracking-[-0.01em]"
          >
            {step.title}
          </h2>
          <p
            id="first-task-walkthrough-body"
            className="text-muted-foreground mt-1.5 text-sm leading-relaxed"
          >
            {step.body}
          </p>
        </div>
        <div className="mt-4 flex items-center gap-2">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="mr-auto text-muted-foreground"
            onClick={() => finish("skipped")}
          >
            Skip setup
          </Button>
          {stepIndex > 0 && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => setStepIndex((current) => current - 1)}
            >
              Back
            </Button>
          )}
          <Button
            type="button"
            size="sm"
            onClick={() => {
              if (stepIndex === STEPS.length - 1) {
                finish("completed");
              } else {
                setStepIndex((current) => current + 1);
              }
            }}
          >
            {stepIndex === STEPS.length - 1 ? "Done" : "Next"}
          </Button>
        </div>
      </div>
    </>,
    document.body,
  );
}
