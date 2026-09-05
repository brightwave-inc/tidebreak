import type { CodeWorkspacePrSnapshot } from "../api/types";
import type { WorkspaceWorkflowModel } from "./workspaceWorkflow";
import { checkSummaryText, prStateChips } from "./prState";
import { STATUS_TEXT } from "./statusTone";

export function workspaceStatusLabel(model: WorkspaceWorkflowModel): string {
  if (model.stage === "loading") return model.summary;
  return [
    model.pr ? `#${model.pr.number}` : null,
    model.localSummary,
    model.prSummary ?? (!model.localSummary ? model.summary : null),
  ]
    .filter(Boolean)
    .join(" · ");
}

/** The header and hovercard explain the same local and hosted facts. */
export function WorkspaceStatusDetails({
  model,
  snapshot,
  error,
}: {
  model: WorkspaceWorkflowModel;
  snapshot: CodeWorkspacePrSnapshot | null;
  error?: string | null;
}) {
  const git = snapshot?.git;
  return (
    <div
      className="flex flex-col gap-2 text-xs"
      data-testid="workspace-status-details"
    >
      <p className="text-muted-foreground leading-5">{model.detail}</p>
      {error && (
        <p role="alert" className="text-critical-foreground leading-5">
          {error} Previous status may be out of date.
        </p>
      )}
      {model.pr && (
        <div className="flex flex-wrap gap-x-3 gap-y-1">
          {prStateChips(model.pr).map((chip) => (
            <span key={chip.key} className={STATUS_TEXT[chip.tone]}>
              {chip.label}
            </span>
          ))}
        </div>
      )}
      {git && (
        <dl className="flex flex-col gap-1.5">
          <div className="flex flex-wrap justify-between gap-x-3 gap-y-1">
            <dt className="text-muted-foreground">Local changes</dt>
            <dd className="min-w-0">
              {git.changed_files} {git.changed_files === 1 ? "file" : "files"} ·{" "}
              {git.staged_files} staged · {git.unstaged_files} unstaged ·{" "}
              {git.untracked_files} untracked
              {git.conflicted_files > 0
                ? ` · ${git.conflicted_files} conflicted`
                : ""}
            </dd>
          </div>
          <div className="flex justify-between gap-3">
            <dt className="text-muted-foreground">Upstream</dt>
            <dd className="min-w-0 truncate font-mono" title={git.upstream}>
              {git.upstream ?? "Not published"}
            </dd>
          </div>
          {git.upstream && (
            <div className="flex justify-between gap-3">
              <dt className="text-muted-foreground">Sync</dt>
              <dd>
                {git.ahead_of_upstream} to push · {git.behind_upstream} to pull
              </dd>
            </div>
          )}
          <div className="flex justify-between gap-3">
            <dt className="text-muted-foreground">Base</dt>
            <dd className="min-w-0 truncate font-mono">
              {model.pr?.base_branch ?? git.base_ref}
            </dd>
          </div>
        </dl>
      )}
      {!git && model.pr?.base_branch && (
        <p className="text-muted-foreground">
          Base: <span className="font-mono">{model.pr.base_branch}</span>
        </p>
      )}
      {model.checks && model.checks.total > 0 && (
        <p className="text-muted-foreground">
          Checks: {checkSummaryText(model.checks)}
        </p>
      )}
    </div>
  );
}
