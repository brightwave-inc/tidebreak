import { useEffect, useState } from "react";
import { createPortal } from "react-dom";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/components/ui/dialog";

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
    body: "Plan stays read-only, Ask confirms actions, Auto handles routine workspace work, and Allow all runs without asking in this chat. Ask is a balanced place to start.",
  },
  {
    id: "attachments",
    target: "tools",
    title: "Add attachments",
    body: "Open Tools to attach files from your computer. This option appears in the desktop app when local file access is available.",
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

  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        if (!nextOpen) finish("skipped");
      }}
    >
      {rect && createPortal(
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
        />,
        document.body,
      )}
      <DialogContent
        withCloseButton={false}
        onInteractOutside={(event) => event.preventDefault()}
        overlayClassName="z-[100] bg-black/45"
        className="z-[102] block max-w-none translate-x-0 translate-y-0 gap-0 rounded-xl border-border bg-popover p-4 text-popover-foreground shadow-xl duration-0 data-[state=closed]:animate-none data-[state=open]:animate-none"
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
          <DialogTitle className="mt-3 text-base tracking-[-0.01em]">
            {step.title}
          </DialogTitle>
          <DialogDescription className="mt-1.5 leading-relaxed">
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
      </DialogContent>
    </Dialog>
  );
}
