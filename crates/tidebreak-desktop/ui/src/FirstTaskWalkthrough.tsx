import { useEffect, useRef, useState } from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { create } from "zustand";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogDescription,
  DialogPortal,
  DialogTitle,
} from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

export const FIRST_TASK_WALKTHROUGH_KEY = "tidebreak.first-task-walkthrough.v2";

export type FirstTaskWalkthroughOutcome = "completed" | "skipped";

/** Composer surface the walkthrough opens so the spotlight has something real to show. */
export type FirstTaskSurface = "model" | "tools" | "permissions" | null;

type WalkthroughStep = {
  id: string;
  surface: FirstTaskSurface;
  /**
   * Boxes that appear for this step, most specific first. The trigger in the
   * composer bar is not a candidate: it is already on screen, and locking onto
   * it hides the row the copy is talking about.
   */
  targets: readonly string[];
  title: string;
  body: string;
};

const STEPS: readonly WalkthroughStep[] = [
  {
    id: "model",
    surface: "model",
    targets: ["model-choice", "model-menu"],
    title: "Choose a model",
    body: "The model menu is open. Models differ in speed, reasoning, and the inputs they understand. The current default is a good place to start.",
  },
  {
    id: "internet",
    surface: "tools",
    targets: ["network", "tools-menu"],
    title: "Set internet access",
    body: "The Tools menu is open. Network is this work's internet setting. Internet access is what web search and current information need. Leave it off when Tidebreak should use only your message and attachments.",
  },
  {
    id: "permissions",
    surface: "permissions",
    targets: ["permissions-ask", "permissions-menu"],
    title: "Choose a permission level",
    body: "The permission menu is open. Choose a level for this work.",
  },
  {
    id: "attachments",
    surface: "tools",
    targets: ["attach-files", "attach-folder", "tools-menu"],
    title: "Add attachments",
    body: "Attach files or a folder from this menu when the task needs your documents. Desktop Tidebreak can read what you attach.",
  },
  {
    id: "starters",
    surface: null,
    targets: ["starter-choice", "starters"],
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
  for (const target of step.targets) {
    const element = document.querySelector<HTMLElement>(targetSelector(target));
    if (element) return element;
  }
  return null;
}

function isPreferredTarget(
  step: WalkthroughStep,
  element: HTMLElement,
): boolean {
  const preferred = step.targets[0];
  return preferred != null && element.matches(targetSelector(preferred));
}

function focusComposer(): void {
  window.requestAnimationFrame(() => {
    document
      .querySelector<HTMLTextAreaElement>("[data-composer-input]")
      ?.focus();
  });
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
  const outcomeRef = useRef<FirstTaskWalkthroughOutcome | null>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  const setSurface = useFirstTaskGuide((state) => state.setSurface);
  const step = STEPS[stepIndex];

  useEffect(() => {
    if (!open) {
      setStepIndex(0);
      setRect(null);
      setSurface(null);
      outcomeRef.current = null;
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
        if (target) {
          observer.disconnect();
          target = null;
        }
        setRect(null);
        if (frames++ < 30) {
          window.requestAnimationFrame(find);
        }
        return;
      }
      if (target !== next) {
        if (target) observer.unobserve(target);
        target = next;
        next.scrollIntoView({ block: "nearest", inline: "nearest" });
        measure(next);
        observer.observe(next);
      }
      // The menu portal can land after a fallback row. Keep looking for the
      // specific box until it mounts or the retry budget runs out.
      if (!isPreferredTarget(step, next) && frames++ < 30) {
        window.requestAnimationFrame(find);
      }
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

  function finish(outcome: FirstTaskWalkthroughOutcome) {
    outcomeRef.current = outcome;
    storeOutcome(outcome);
    setSurface(null);
    onClose(outcome);
    focusComposer();
  }

  const dialogWidth =
    typeof window === "undefined" ? 320 : Math.min(320, window.innerWidth - 24);
  const above = rect !== null && rect.top >= 220;
  const dialogLeft =
    typeof window === "undefined"
      ? 12
      : rect
        ? Math.max(
            12,
            Math.min(
              rect.left + rect.width / 2 - dialogWidth / 2,
              window.innerWidth - dialogWidth - 12,
            ),
          )
        : Math.max(12, (window.innerWidth - dialogWidth) / 2);
  const dialogTop =
    typeof window === "undefined"
      ? 0
      : rect
        ? above
          ? rect.top - 12
          : rect.bottom + 12
        : window.innerHeight / 2;
  const inset = 4;

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (next) return;
        // Escape and programmatic close settle as a skip unless a button
        // already chose completed/skipped through finish().
        if (outcomeRef.current) return;
        finish("skipped");
      }}
    >
      <DialogPortal>
        {rect && (
          <div
            className="pointer-events-none fixed inset-0 z-[100]"
            aria-hidden="true"
          >
            <div
              className="pointer-events-auto absolute inset-0 bg-black/50"
              style={{ clipPath: clipPathFor(rect, inset) }}
            />
            <div
              data-first-task-ring
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
        <DialogPrimitive.Content
          aria-describedby="first-task-walkthrough-body"
          className={cn(
            "fixed z-[102] max-w-none rounded-xl border border-border bg-popover p-4 text-popover-foreground shadow-xl outline-none",
            "data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0",
          )}
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
          // Spotlight hole clicks must not dismiss the card; only Skip, Done,
          // and Escape close it.
          onPointerDownOutside={(event) => event.preventDefault()}
          onInteractOutside={(event) => event.preventDefault()}
          onCloseAutoFocus={(event) => {
            event.preventDefault();
            focusComposer();
          }}
        >
          <div aria-live="polite" aria-atomic="true">
            <div className="flex items-center justify-between gap-3 text-xs text-muted-foreground">
              <span>Set up your first task</span>
              <span>
                {stepIndex + 1} of {STEPS.length}
              </span>
            </div>
            <DialogTitle
              id="first-task-walkthrough-title"
              className="mt-3 text-base font-semibold tracking-[-0.01em]"
            >
              {step.title}
            </DialogTitle>
            <DialogDescription
              id="first-task-walkthrough-body"
              className="text-muted-foreground mt-1.5 text-sm leading-relaxed"
            >
              {step.body}
            </DialogDescription>
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
        </DialogPrimitive.Content>
      </DialogPortal>
    </Dialog>
  );
}
