import { ChevronDown, GitPullRequest } from "lucide-react";

import type { PullRequestDigest } from "../api/types";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { openExternal } from "@/host";
import { FOCUS_RING } from "./interactive";
import {
  prBarActionLabel,
  prBarModel,
  prBarPrompt,
  type PrBarAction,
  type PrBarTone,
} from "./prActions";
import { useCodeUiStore } from "./CodeUiStore";

const TONE_CLASS: Record<PrBarTone, string> = {
  ready:
    "border-success-border bg-success-background text-success-foreground",
  pending: "border-border bg-muted/40 text-foreground",
  failing:
    "border-critical-border bg-critical-background text-critical-foreground",
  conflict:
    "border-warning-border bg-warning-background text-warning-foreground",
  draft: "border-border bg-muted/40 text-muted-foreground",
  merged: "border-border bg-muted/40 text-foreground",
  closed:
    "border-critical-border bg-critical-background text-critical-foreground",
};

/**
 * Persistent PR strip above the inspector tabs.
 *
 * Status and the check count stay visible on Files and Source control. Each
 * action writes a prompt into the composer rather than calling GitHub here.
 */
export function PrActionBar({ pr }: { pr: PullRequestDigest }) {
  const offerComposerPrompt = useCodeUiStore((state) => state.offerComposerPrompt);
  const model = prBarModel(pr);
  const [primary, ...rest] = model.actions;

  function insert(action: PrBarAction) {
    offerComposerPrompt(prBarPrompt(action, pr));
    window.requestAnimationFrame(() => {
      document
        .querySelector<HTMLTextAreaElement>("[data-composer-input]")
        ?.focus();
    });
  }

  return (
    <div
      className={cn(
        "flex shrink-0 items-center gap-2 border-b px-2 py-1.5",
        TONE_CLASS[model.tone],
      )}
      data-testid="pr-action-bar"
    >
      {model.url ? (
        <a
          href={model.url}
          className={cn(
            "shrink-0 rounded-sm font-mono text-[11px] font-semibold underline-offset-2 hover:underline",
            FOCUS_RING,
          )}
          title={pr.title ?? `#${model.number}`}
          onClick={(event) => {
            event.preventDefault();
            void openExternal(model.url!).catch(() => undefined);
          }}
        >
          #{model.number}
        </a>
      ) : (
        <span className="shrink-0 font-mono text-[11px] font-semibold">
          #{model.number}
        </span>
      )}
      <GitPullRequest className="size-3 shrink-0" aria-hidden="true" />
      <p className="min-w-0 flex-1 truncate text-[11px] font-medium">
        {model.status}
        {model.checks.total > 0 && (
          <span className="font-normal opacity-80">
            {" "}
            · {model.checks.total}{" "}
            {model.checks.total === 1 ? "check" : "checks"}
          </span>
        )}
      </p>
      {primary && (
        <div className="flex shrink-0 items-center">
          <Button
            type="button"
            size="sm"
            className={cn("h-6 px-2 text-[11px]", rest.length > 0 && "rounded-r-none")}
            onClick={() => insert(primary)}
          >
            {prBarActionLabel(primary)}
          </Button>
          {rest.length > 0 && (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  type="button"
                  size="sm"
                  className="h-6 rounded-l-none border-l border-l-background/20 px-1"
                  aria-label="More pull request actions"
                >
                  <ChevronDown className="size-3.5" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-44">
                {rest.map((action) => (
                  <DropdownMenuItem
                    key={action}
                    onSelect={() => insert(action)}
                  >
                    {prBarActionLabel(action)}
                  </DropdownMenuItem>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </div>
      )}
    </div>
  );
}
