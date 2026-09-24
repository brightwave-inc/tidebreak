import { CircleCheck, TriangleAlert, X } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";

import type { DiagnosticsSaveState } from "./desktopLifecycle";
import { Notice } from "@/components/ui/notice";
import { friendlyErrorMessage } from "@/lib/utils";

type UncleanExitNoticeProps = {
  save: DiagnosticsSaveState;
  onSave: () => void;
  onDismiss: () => void;
};

/**
 * The quiet notice after a launch that followed an unclean exit: a crash, a
 * force quit, or a power loss. It offers a diagnostics report, saved to a
 * file the person picks, and nothing else happens unless they share it.
 */
export function UncleanExitNotice({
  save,
  onSave,
  onDismiss,
}: UncleanExitNoticeProps) {
  const saving = save.status === "saving";
  // Shaped like the update card it stacks with; the warning icon carries the
  // tone.
  return (
    <aside
      className="relative rounded-xl border border-border bg-popover p-4 text-popover-foreground shadow-lg"
      aria-label="Tidebreak quit unexpectedly"
    >
      <button
        type="button"
        className="absolute top-2.5 right-2.5 grid size-7 cursor-pointer place-items-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-3 focus-visible:ring-ring/25"
        aria-label="Dismiss notice"
        onClick={onDismiss}
      >
        <X className="size-4" aria-hidden="true" />
      </button>

      <div className="flex items-start gap-3 pr-7">
        <TriangleAlert
          className="mt-0.5 size-4 shrink-0 text-warning"
          aria-hidden="true"
        />
        <div className="min-w-0">
          <p className="text-md font-semibold">Tidebreak quit unexpectedly</p>
          <p className="mt-1 text-sm text-muted-foreground">
            A diagnostics report can show what went wrong. It stays on this
            computer unless you share it.
          </p>
        </div>
      </div>

      <div className="mt-4 flex flex-wrap items-center gap-x-3 gap-y-2">
        <Button type="button" size="sm" disabled={saving} onClick={onSave}>
          {saving && <Spinner aria-hidden="true" className="text-current" />}
          {saving ? "Saving report…" : "Save diagnostics report"}
        </Button>
        {save.status === "saved" && (
          <p
            className="flex items-center gap-1.5 text-sm text-success-foreground"
            role="status"
          >
            <CircleCheck className="size-3.5" aria-hidden="true" />
            Report saved
          </p>
        )}
      </div>
      {save.status === "failed" && (
        <Notice tone="critical" className="mt-2">
          {friendlyErrorMessage(save.error, "Try again in a moment.")}
        </Notice>
      )}
    </aside>
  );
}
