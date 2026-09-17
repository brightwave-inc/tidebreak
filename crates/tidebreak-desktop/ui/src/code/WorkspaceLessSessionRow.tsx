import { recoveryDigest } from "./sessionRecovery";
import { useRecoveryDelay } from "./useRecoveryDelay";
import type { CodeSessionDigest } from "../api/types";
import { cn } from "../lib/utils";
import { HARNESS_ICONS } from "./HarnessPicker";
import { FOCUS_RING_INSET, HOVER_TINT } from "./interactive";
import { HARNESS_LABELS } from "./labels";
import { SessionStateGlyph } from "./WorkspaceCard";
import { formatCompactAge, sessionActivityLineLabel } from "./workspaceCards";
import { pointerSelectIntent } from "./workspaceSelection";
import { sessionTreeWaitLabel } from "./sessionTree";

/**
 * An index row for a conversation that has no repository workspace.
 *
 * It reads like a workspace card: state glyph and title on the first line,
 * then source and status with the engine mark and age on the second. It has
 * no selection of its own — nothing bulk applies to these sessions yet — but
 * it must not hijack a selection gesture either, so a modifier click is
 * absorbed rather than treated as an open.
 */
export function WorkspaceLessSessionRow({
  digest,
  onOpen,
  active = false,
  density = "detailed",
  nested = [],
  activeSessionId = null,
  childrenByParent,
}: {
  digest: CodeSessionDigest;
  active?: boolean;
  density?: "compact" | "detailed";
  onOpen: (sessionId: string) => void;
  nested?: CodeSessionDigest[];
  activeSessionId?: string | null;
  childrenByParent?: ReadonlyMap<string, CodeSessionDigest[]>;
}) {
  digest = recoveryDigest(digest);
  const origin = digest.external_origin;
  const channel = origin?.external_key.split("/")[1];
  const source =
    origin?.channel_kind === "slack"
      ? channel?.startsWith("D")
        ? "Slack direct message"
        : "Slack channel"
      : origin?.channel_kind;
  const title =
    digest.title?.trim() ||
    (source ? `${source} conversation` : "Untitled conversation");
  const recovering = digest.attention.state.type === "fenced";
  const showRecovery = useRecoveryDelay(recovering);
  // The same copy as a workspace card's activity line: the live tool subject
  // while running, the recap once parked.
  const status =
    recovering && !showRecovery
      ? ""
      : (sessionTreeWaitLabel(digest.wait) ?? sessionActivityLineLabel(digest));
  const harnessKind = digest.harness_kind;
  const HarnessIcon = harnessKind ? HARNESS_ICONS[harnessKind] : null;
  const age = digest.trigger_target_at
    ? formatCompactAge(digest.trigger_target_at)
    : null;
  const detail = [source, status].filter(Boolean).join(" · ");
  const row = (
    <button
      type="button"
      data-workspace-card=""
      className={cn(
        "flex w-full min-w-0 cursor-pointer items-start gap-2 rounded-xl px-2.5 py-2 text-left hover:bg-muted",
        FOCUS_RING_INSET,
        HOVER_TINT,
        active && "bg-muted",
      )}
      aria-label={[
        title,
        source,
        status,
        harnessKind ? HARNESS_LABELS[harnessKind] : undefined,
      ]
        .filter(Boolean)
        .join(", ")}
      aria-current={active ? "page" : undefined}
      onMouseDown={(event) => {
        // Same guard as the workspace card: a shift-click must not extend
        // the page's text selection mid-gesture.
        if (pointerSelectIntent(event) !== "open") event.preventDefault();
      }}
      onClick={(event) => {
        if (pointerSelectIntent(event) !== "open") {
          event.preventDefault();
          return;
        }
        onOpen(digest.session);
      }}
    >
      <span className="mt-1 shrink-0">
        <SessionStateGlyph digest={digest} />
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="truncate text-md font-medium leading-5" title={title}>
          {title}
        </span>
        {density === "detailed" && (detail || HarnessIcon || age) && (
          <span className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
            <span className="min-w-0 flex-1 truncate" title={detail}>
              {detail}
            </span>
            {harnessKind && HarnessIcon && (
              // A brand mark, so it keeps its own identity and its own slot,
              // exactly as on the workspace card's activity line.
              <span title={HARNESS_LABELS[harnessKind]} className="shrink-0">
                <HarnessIcon className="size-3 opacity-70" aria-hidden />
              </span>
            )}
            {age && (
              <span className="shrink-0 tabular-nums">
                {age === "now" ? "now" : age}
              </span>
            )}
          </span>
        )}
      </span>
    </button>
  );
  if (nested.length === 0) return row;
  return (
    <div className="flex min-w-0 flex-col">
      {row}
      <ul className="ml-4 border-l border-border-subtle pl-1">
        {nested.map((child) => (
          <li key={child.session}>
            <WorkspaceLessSessionRow
              digest={child}
              onOpen={onOpen}
              active={
                activeSessionId ? child.session === activeSessionId : false
              }
              density={density}
              nested={childrenByParent?.get(child.session) ?? []}
              childrenByParent={childrenByParent}
              activeSessionId={activeSessionId}
            />
          </li>
        ))}
      </ul>
    </div>
  );
}
