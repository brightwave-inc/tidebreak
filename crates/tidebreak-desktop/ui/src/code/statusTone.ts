import type { Attention, CodeSessionDigest } from "../api/types";

/**
 * The one place code mode turns a state into a color.
 *
 * Two things were being decided together and kept coming out differently. A
 * *tone* is what a state means — ready, warning, critical. A *rung* is which
 * shade of that tone a given surface should paint, and it follows from what is
 * being drawn: a word, a standalone glyph, a filled dot, a pill. Every surface
 * used to pick both for itself, so the same state drew as `text-critical` on
 * one row of a workspace card and `text-critical-foreground` on the next, and
 * the card and the workflow control disagreed on four of five tones.
 *
 * Surfaces now choose a tone and say what they are painting. The rung is not
 * theirs to pick.
 *
 * Status tones are reserved for status. File types, repo swatches, and engine
 * badges are identity, not state: those use the `--icon-*` family instead, so
 * a `.json` file never reads as a warning.
 */

/**
 * The status vocabulary. A superset of `WorkspaceWorkflowTone` sharing its
 * member names, so a workflow model's tone indexes these maps directly.
 */
export type StatusTone =
  | "neutral"
  | "running"
  | "ready"
  | "pending"
  | "warning"
  | "critical"
  | "merged";

/**
 * Label text, and icons sitting inline beside it. The `-foreground` rung is
 * the readable one: it carries the hue at text contrast rather than at the
 * full-strength value a lone glyph needs.
 */
export const STATUS_TEXT: Record<StatusTone, string> = {
  neutral: "text-muted-foreground",
  running: "text-live-foreground",
  ready: "text-success-foreground",
  pending: "text-info-foreground",
  warning: "text-warning-foreground",
  critical: "text-critical-foreground",
  merged: "text-merged-foreground",
};

/**
 * A glyph carrying the state on its own — a PR mark, an alert circle. Nothing
 * next to it says what it is, so it takes the full-strength value.
 */
export const STATUS_MARK: Record<StatusTone, string> = {
  neutral: "text-muted-foreground",
  running: "text-live",
  ready: "text-success",
  pending: "text-info",
  warning: "text-warning",
  critical: "text-critical",
  merged: "text-merged",
};

/** A filled dot. Same reasoning as a mark, in a background rather than ink. */
export const STATUS_DOT: Record<StatusTone, string> = {
  neutral: "bg-muted-foreground/60",
  running: "bg-live",
  ready: "bg-success",
  pending: "bg-info",
  warning: "bg-warning",
  critical: "bg-critical",
  merged: "bg-merged",
};

/** A pill: a tinted field with text that has to stay legible on it. */
export const STATUS_CHIP: Record<StatusTone, string> = {
  neutral: "bg-muted text-muted-foreground",
  running: "bg-live-background text-live-foreground-muted",
  ready: "bg-success-background text-success-foreground-muted",
  pending: "bg-info-background text-info-foreground-muted",
  warning: "bg-warning-background text-warning-foreground-muted",
  critical: "bg-critical-background text-critical-foreground-muted",
  merged: "bg-merged-background text-merged-foreground-muted",
};

/**
 * Motion, for the tones that mean something is happening right now.
 *
 * Running paints the live ramp — the one accent reserved for an agent doing
 * work this second — while pending stays on info's blue: it is only waiting
 * to be told. Motion still reinforces the difference; callers spread this
 * onto the same element they tone.
 */
export const STATUS_MOTION: Partial<Record<StatusTone, string>> = {
  running: "animate-pulse",
};

/**
 * What an attention state means, as a color.
 *
 * `working` is a tone here rather than the absence of one. It used to render
 * nothing at all, which left the single most common live state — an agent
 * mid-turn — looking exactly like an idle one.
 */
export function attentionStatusTone(attention: Attention): StatusTone {
  switch (attention.state.type) {
    case "working":
      return "running";
    case "needs_you":
      return "critical";
    case "stalled":
    case "fenced":
      return "warning";
    case "manual":
      return "pending";
    case "done_unreviewed":
      return "neutral";
    // Resting, not finished: nothing for the reader to act on, and nothing
    // that should read as a completed result they have yet to look at.
    case "idle":
      return "neutral";
  }
}

/**
 * A whole session's tone: what it wants from the reader, or that it is busy.
 *
 * Attention outranks lifecycle. A session can be `running` and still need an
 * answer, and the need is the thing worth coloring.
 */
export function digestStatusTone(
  digest: CodeSessionDigest | undefined,
): StatusTone {
  if (!digest) return "neutral";
  if (digest.attention.state.type !== "working") {
    return attentionStatusTone(digest.attention);
  }
  return digest.lifecycle === "running" ? "running" : "neutral";
}
