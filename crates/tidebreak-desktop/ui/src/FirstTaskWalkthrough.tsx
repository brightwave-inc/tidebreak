import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { Button } from "@/components/ui/button";

export const FIRST_TASK_WALKTHROUGH_KEY =
  "tidebreak.first-task-walkthrough.v1";

export type FirstTaskWalkthroughOutcome = "completed" | "skipped";

type WalkthroughStep = {
  id: string;
  target: string;
  title: string;
  body: string;
};

const STEPS: readonly WalkthroughStep[] = [
  {
    id: "model",
    target: "model",
    title: "Choose a model",
    body: "Models differ in speed, reasoning, and the inputs they understand. The current default is a good place to start.",
  },
  {
    id: "internet",
    target: "tools",
    title: "Set internet access",
    body: "Open Tools and choose Network when the task needs websites or current information. Leave it off when Tidebreak should use only your message and attachments.",
  },
  {
    id: "permissions",
    target: "permissions",
    title: "Choose a permission level",
    body: "Ask confirms actions, Auto handles routine workspace work, and Allow all runs without asking in this chat. Ask is a safe place to start.",
  },
  {
    id: "attachments",
    target: "tools",
    title: "Add attachments",
    body: "Open Tools to attach files for this task or connect a folder when the agent should work across a collection of files.",
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

function focusComposer(): void {
  window.requestAnimationFrame(() => {
    document
      .querySelector<HTMLTextAreaElement>("[data-composer-input]")
      ?.focus();
  });
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
  const dialogRef = useRef<HTMLDivElement>(null);
  const step = STEPS[stepIndex];

  useEffect(() => {
    if (!open) {
      setStepIndex(0);
      setRect(null);
      return;
    }

    const target = document.querySelector<HTMLElement>(
      targetSelector(step.target),
    );
    if (!target) {
      setRect(null);
      return;
    }

    target.scrollIntoView({ block: "nearest", inline: "nearest" });
    const update = () => {
      const next = target.getBoundingClientRect();
      setRect({
        top: next.top,
        left: next.left,
        right: next.right,
        bottom: next.bottom,
        width: next.width,
        height: next.height,
      });
    };
    update();

    const observer = new ResizeObserver(update);
    observer.observe(target);
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
    };
  }, [open, step.target]);

  useEffect(() => {
    if (!open) return;
    dialogRef.current?.focus();
  }, [open, stepIndex, rect]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      finish("skipped");
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  });

  function finish(outcome: FirstTaskWalkthroughOutcome) {
    storeOutcome(outcome);
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

  return createPortal(
    <div className="fixed inset-0 z-[100]" aria-live="polite">
      <div className="absolute inset-0 bg-black/45" aria-hidden="true" />
      {rect && (
        <div
          className="pointer-events-none fixed z-[101] rounded-[10px] ring-2 ring-[var(--brightwave)] ring-offset-2 ring-offset-transparent"
          aria-hidden="true"
          style={{
            top: rect.top - 4,
            left: rect.left - 4,
            width: rect.width + 8,
            height: rect.height + 8,
            boxShadow: "0 0 0 9999px rgb(0 0 0 / 0.01)",
          }}
        />
      )}
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={`first-task-walkthrough-${step.id}`}
        tabIndex={-1}
        className="fixed z-[102] rounded-xl border border-border bg-popover p-4 text-popover-foreground shadow-xl outline-none"
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
        <div className="flex items-center justify-between gap-3 text-xs text-muted-foreground">
          <span>Set up your first task</span>
          <span>
            {stepIndex + 1} of {STEPS.length}
          </span>
        </div>
        <h2
          id={`first-task-walkthrough-${step.id}`}
          className="mt-3 text-base font-semibold tracking-[-0.01em]"
        >
          {step.title}
        </h2>
        <p className="mt-1.5 text-sm leading-relaxed text-muted-foreground">
          {step.body}
        </p>
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
    </div>,
    document.body,
  );
}
