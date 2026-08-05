import { FileText } from "lucide-react";

import type { AgentRun } from "./api";

/** Beyond this many, the rest collapse into a count rather than wrapping on. */
const MAX_VISIBLE_OUTPUTS = 6;

/**
 * The files a background agent handed over, named by the agent itself.
 *
 * A background run's deliverable is not text the host paraphrases: the run
 * writes files under `output/`, the scan publishes them under their own
 * filenames, and `done` submits that set. So this is the whole result surface —
 * one pill per file, each opening the output it names.
 */
export function SubmittedOutputPills({
  outputs,
  onOpenOutput,
}: {
  outputs: AgentRun["submitted_outputs"];
  /** Open one submitted output; without it the pills are labels, not links. */
  onOpenOutput?: (outputId: string) => void;
}) {
  if (outputs.length === 0) return null;
  const visible = outputs.slice(0, MAX_VISIBLE_OUTPUTS);
  const hidden = outputs.length - visible.length;

  return (
    <div className="flex flex-wrap items-center gap-1.5" aria-label="Submitted files">
      {visible.map((output) => (
        <SubmittedOutputPill
          key={output.output_id}
          filename={output.filename}
          onOpen={onOpenOutput ? () => onOpenOutput(output.output_id) : undefined}
        />
      ))}
      {hidden > 0 && (
        <span className="text-muted-foreground text-xs">+{hidden} more</span>
      )}
    </div>
  );
}

function SubmittedOutputPill({
  filename,
  onOpen,
}: {
  filename: string;
  onOpen?: () => void;
}) {
  const body = (
    <>
      <FileText className="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
      <span className="min-w-0 truncate">{filename}</span>
    </>
  );
  const frame =
    "bg-background inline-flex max-w-56 min-w-0 items-center gap-1.5 rounded-md border px-2 py-1 text-xs";
  if (!onOpen) {
    return <span className={frame}>{body}</span>;
  }
  return (
    <button
      type="button"
      className={`${frame} hover:bg-muted/40 cursor-pointer transition-colors`}
      onClick={onOpen}
      aria-label={`Open output ${filename}`}
    >
      {body}
    </button>
  );
}
