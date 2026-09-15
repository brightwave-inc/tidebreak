import { Eye, ExternalLink, Zap } from "lucide-react";

import type { TriggerTurnContext } from "../generated/wire";
import { ToolCardShell } from "../ToolCardShell";
import { cn } from "@/lib/utils";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "./interactive";
import { openExternal } from "@/host";
import { STATUS_TEXT, type StatusTone } from "./statusTone";

/**
 * The phrase and tone for one fired condition, aligned with the PR state
 * chips so the event in the transcript reads the same as the status beside
 * it. The phrase names the pull request, mirroring the server's own
 * condition descriptions.
 */
export function triggerEventCopy(context: TriggerTurnContext): {
  phrase: string;
  tone: StatusTone;
} {
  const pr = `#${context.pr_number}`;
  switch (context.condition) {
    case "checks_failed":
      return { phrase: `Checks failed on ${pr}`, tone: "critical" };
    case "conflicts":
      return { phrase: `${pr} has merge conflicts`, tone: "critical" };
    case "changes_requested":
      return { phrase: `Changes requested on ${pr}`, tone: "warning" };
    case "review_required":
      return { phrase: `${pr} is waiting on review`, tone: "pending" };
    case "behind":
      return { phrase: `${pr} is behind its base`, tone: "warning" };
    case "ready_to_merge":
      return { phrase: `${pr} is ready to merge`, tone: "ready" };
    case "merged":
      return { phrase: `${pr} merged`, tone: "merged" };
    case "closed":
      return { phrase: `${pr} closed without merging`, tone: "neutral" };
    case "pr_opened":
      return { phrase: `Pull request ${pr} opened`, tone: "pending" };
    case "pr_updated":
      return { phrase: `${pr} has a new head`, tone: "neutral" };
  }
}

/** One line naming the event, for surfaces smaller than the card. */
export function triggerEventSummary(context: TriggerTurnContext): string {
  return triggerEventCopy(context).phrase;
}

function sourceLabel(context: TriggerTurnContext): string {
  return context.source === "watch" ? "Watch" : "Trigger";
}

/**
 * A pull-request event delivered to the agent, drawn as the event it is.
 *
 * A trigger fire or a watch fix turn arrives as a turn nobody typed. Showing
 * its rendered instruction as a person's message buries the one fact the
 * reader wants — what happened on the pull request — under prose written for
 * the harness. This card leads with the condition and the pull request, and
 * folds the delivered instruction, the failing checks, and the head behind
 * the expand, following the tool row's boxless idiom.
 */
export function TriggerEventCard({
  context,
  message,
}: {
  context: TriggerTurnContext;
  /** The exact instruction the agent received, shown expanded. */
  message: string;
}) {
  const copy = triggerEventCopy(context);
  const failing = context.failing_checks ?? [];
  return (
    <ToolCardShell
      icon={context.source === "watch" ? <Eye /> : <Zap />}
      title={
        <>
          <span
            className={cn("shrink-0 font-semibold", STATUS_TEXT[copy.tone])}
          >
            {copy.phrase}
          </span>
          {context.pr_title && (
            <span className="text-muted-foreground min-w-0 flex-1 truncate">
              {context.pr_title}
            </span>
          )}
        </>
      }
      titleClassName="flex items-center gap-2"
      trailing={
        <>
          <span>{sourceLabel(context)}</span>
          <span className="sr-only">
            {sourceLabel(context)} event: {triggerEventSummary(context)}
          </span>
        </>
      }
      label={`${sourceLabel(context)} event: ${triggerEventSummary(context)}`}
      announce={false}
    >
      <div className="flex flex-col gap-2 text-xs">
        {context.pr_url && (
          <PullRequestLink
            url={context.pr_url}
            number={context.pr_number}
            title={context.pr_title ?? null}
          />
        )}
        {failing.length > 0 && (
          <div className="flex flex-col gap-0.5">
            <p className="text-muted-foreground">Failing checks</p>
            {failing.map((check) => (
              <FailingCheckRow
                key={check.name}
                name={check.name}
                url={check.url ?? null}
              />
            ))}
          </div>
        )}
        {context.head_sha && (
          <p className="text-muted-foreground">
            Head{" "}
            <span className="font-mono">{context.head_sha.slice(0, 9)}</span>
          </p>
        )}
        <div>
          <p className="text-muted-foreground mb-0.5">Delivered to the agent</p>
          <p className="text-foreground/80 break-words whitespace-pre-wrap">
            {message}
          </p>
        </div>
      </div>
    </ToolCardShell>
  );
}

function PullRequestLink({
  url,
  number,
  title,
}: {
  url: string;
  number: number;
  title: string | null;
}) {
  return (
    <a
      href={url}
      className={cn(
        "hover:bg-muted/50 -mx-1 flex w-fit max-w-full cursor-pointer items-center gap-1.5 rounded-md px-1 py-0.5",
        FOCUS_RING_TIGHT,
        HOVER_TINT,
      )}
      onClick={(event) => {
        event.preventDefault();
        void openExternal(url).catch(() => undefined);
      }}
    >
      <span className="font-mono">#{number}</span>
      {title && <span className="min-w-0 truncate">{title}</span>}
      <ExternalLink className="text-muted-foreground size-3 shrink-0" />
    </a>
  );
}

function FailingCheckRow({ name, url }: { name: string; url: string | null }) {
  const body = (
    <span className={cn("min-w-0 truncate font-mono", STATUS_TEXT.critical)}>
      {name}
    </span>
  );
  if (!url) {
    return <div className="flex items-center gap-1.5 py-0.5">{body}</div>;
  }
  return (
    <a
      href={url}
      className={cn(
        "hover:bg-muted/50 -mx-1 flex w-fit max-w-full cursor-pointer items-center gap-1.5 rounded-md px-1 py-0.5",
        FOCUS_RING_TIGHT,
        HOVER_TINT,
      )}
      onClick={(event) => {
        event.preventDefault();
        void openExternal(url).catch(() => undefined);
      }}
    >
      {body}
      <ExternalLink className="text-muted-foreground size-3 shrink-0" />
    </a>
  );
}
