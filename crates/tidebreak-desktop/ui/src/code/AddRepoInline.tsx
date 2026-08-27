import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { ParentDirField } from "./AddRepoPalette";
import type { AddRepoInlineState } from "./useAddRepoInline";

/**
 * What one submit is about to do with what has been typed.
 *
 * Nothing typed reads nothing: the placeholder already names the three
 * shapes, and repeating it under the field would say it twice.
 */
function readsAs(state: AddRepoInlineState): string | null {
  if (state.kind === "url") return "Clones this remote, then creates.";
  if (state.kind === "github") return "Clones it from GitHub, then creates.";
  if (state.kind === "path") return "Registers the checkout at this path.";
  return null;
}

/**
 * Register or clone a repository without leaving the new-workspace composer.
 *
 * A registered repository does nothing on its own, so the palette already
 * hands one straight to this dialog. This field closes the last seam the
 * other way: the first message is already typed, and adding the repository is
 * the same submit that creates the workspace.
 *
 * One field reads all three sources rather than asking which one first. The
 * palette still owns the full flow — browsing GitHub, retrying a clone, a
 * clone resumed from a notification — and this is the short path through it.
 */
export function AddRepoInline({
  state,
  submitLabel,
}: {
  state: AddRepoInlineState;
  /** What this submit finishes with, which the dialog decides. */
  submitLabel: string;
}) {
  const cloning = state.phase !== null;
  return (
    <form
      className="flex flex-col gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        // The dialog's own form is this popover's React parent. Without this
        // the same keystroke would also try to create.
        event.stopPropagation();
        state.submit();
      }}
    >
      {/* `text-sm` because this form embeds the palette's destination field,
          and one form must not carry two label sizes. */}
      <span className="text-sm font-medium">Repository</span>
      <div className="flex gap-2">
        <Input
          value={state.value}
          onChange={(event) => state.setValue(event.target.value)}
          placeholder="Path, Git URL, or owner/repo"
          aria-label="Repository path or URL"
          disabled={state.busy}
          autoFocus
        />
        {state.canBrowse && (
          <Button
            type="button"
            variant="outline"
            className="shrink-0"
            onClick={state.browse}
            disabled={state.busy}
          >
            Browse
          </Button>
        )}
      </div>
      {/* The reading of the value sits under the value, not under the
          destination the value happens to also need. */}
      {state.blocked ? (
        <p className="text-critical text-xs" data-testid="add-repo-blocked">
          {state.blocked}
        </p>
      ) : (
        readsAs(state) && (
          <p className="text-muted-foreground text-xs">{readsAs(state)}</p>
        )
      )}
      {state.needsDestination && !state.blocked && (
        <ParentDirField
          value={state.parentDir}
          busy={state.busy}
          canBrowse={state.canBrowse}
          blocked={false}
          defaultsProbeFailed={state.defaultsProbeFailed}
          defaultsBusy={state.defaultsBusy}
          onChange={state.setParentDir}
          onBrowse={state.browseDestination}
          onRetryDefaults={state.retryDefaults}
        />
      )}
      {cloning && (
        // A hairline separates what was asked for from what is happening;
        // without it the phase reads as a note on the destination field.
        <div className="flex flex-col gap-1.5 border-t border-border-subtle pt-2">
          <div className="flex items-center justify-between gap-2">
            <span
              className="text-muted-foreground truncate text-xs"
              data-testid="add-repo-clone-phase"
            >
              {state.phase}
            </span>
            {state.percent !== null && (
              <span className="text-muted-foreground shrink-0 font-mono text-xs">
                {Math.round(state.percent)}%
              </span>
            )}
          </div>
          <Progress
            value={state.percent ?? 0}
            style={{ forcedColorAdjust: "none" }}
          />
          <p className="text-muted-foreground text-xs">
            This clone keeps running if you close this window.
          </p>
        </div>
      )}
      {state.error && (
        <p className="text-critical text-xs" data-testid="add-repo-error">
          {state.error}
        </p>
      )}
      <Button type="submit" className="self-start" disabled={!state.canSubmit}>
        {state.busy ? (cloning ? "Cloning…" : "Adding…") : submitLabel}
      </Button>
    </form>
  );
}
