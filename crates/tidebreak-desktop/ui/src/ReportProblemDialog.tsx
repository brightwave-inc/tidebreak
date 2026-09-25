import { useEffect, useRef, useState } from "react";
import { Bot, ExternalLink } from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Notice } from "@/components/ui/notice";
import { Spinner } from "@/components/ui/spinner";
import { friendlyErrorMessage } from "@/lib/utils";
import { MENU_COMMAND_EVENT, useNativeHostEvent } from "./nativeMenu";
import { openInBrowser } from "./openInBrowser";
import {
  problemReportFacts,
  problemReportIssueUrl,
  revealLogsDirectory,
  saveDiagnosticsReport,
  useReportProblem,
  type ProblemReportFacts,
} from "./reportProblem";
import { releaseNotesUrl } from "./UpdateReadyCard";

/** Where the dialog is in its one job. */
export type ReportProblemPhase =
  | { step: "idle" }
  | { step: "saving" }
  | { step: "saved" }
  | { step: "failed"; message: string };

const SAVE_FAILED = "Could not save the diagnostics report.";

export type ReportProblemDialogProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /**
   * Hand the report to an agent in this workspace instead (decision 81).
   * Offered only where a session's debug details exist.
   */
  onAskAgent?: () => void;
  /** Injectable for stories and tests; default to the native shell's. */
  readFacts?: () => Promise<ProblemReportFacts>;
  saveReport?: () => Promise<boolean>;
  openUrl?: (url: string) => Promise<void>;
  /** Start in a later phase, for stories. */
  initialPhase?: ReportProblemPhase;
};

/**
 * Report a problem: save the diagnostics report, then open a GitHub issue
 * with the version, operating system, and architecture filled in.
 *
 * Every way in (the Help menu, the boot screen, a crash, Settings →
 * Updates, a workspace's menu) opens this one dialog, so a bug report looks
 * the same wherever the person found the problem. The report stays on this
 * computer until they attach it; the issue's address carries only the
 * three facts, which the dialog shows before anything opens.
 */
export function ReportProblemDialog({
  open,
  onOpenChange,
  onAskAgent,
  readFacts = problemReportFacts,
  saveReport = saveDiagnosticsReport,
  openUrl = openInBrowser,
  initialPhase = { step: "idle" },
}: ReportProblemDialogProps) {
  const [facts, setFacts] = useState<ProblemReportFacts | null>(null);
  const [phase, setPhase] = useState<ReportProblemPhase>(initialPhase);
  const primary = useRef<HTMLButtonElement>(null);
  // Each opening starts over from here, not from wherever the last one ended.
  const firstPhase = useRef(initialPhase);

  useEffect(() => {
    if (!open) return;
    setPhase(firstPhase.current);
    let cancelled = false;
    void readFacts().then((next) => {
      if (!cancelled) setFacts(next);
    });
    return () => {
      cancelled = true;
    };
  }, [open, readFacts]);

  const issueUrl = problemReportIssueUrl(
    facts ?? { version: null, os: null, arch: null },
  );
  const saving = phase.step === "saving";
  const prefill = factsLine(facts);

  async function saveAndOpenIssue() {
    setPhase({ step: "saving" });
    let saved: boolean;
    try {
      saved = await saveReport();
    } catch (error) {
      setPhase({
        step: "failed",
        message: friendlyErrorMessage(error, SAVE_FAILED),
      });
      return;
    }
    if (!saved) {
      // They closed the save dialog: nothing happened, so nothing opens.
      setPhase({ step: "idle" });
      return;
    }
    setPhase({ step: "saved" });
    await openUrl(issueUrl);
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="max-w-lg"
        // Land on the one thing the dialog is for; nothing here is
        // destructive, and the save asks where the file goes first.
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          primary.current?.focus();
        }}
      >
        <DialogHeader>
          <DialogTitle>Report a problem</DialogTitle>
          <DialogDescription>
            Save a diagnostics report, then describe what happened in a new
            GitHub issue and attach the report there.
          </DialogDescription>
        </DialogHeader>

        <p className="text-sm text-muted-foreground">
          The report holds recent logs and a snapshot of this app. It leaves out
          your conversations, files, and credentials. Logs can name folders on
          this computer, so look it over before you share it.
        </p>
        <div className="rounded-md bg-muted px-3 py-2">
          {prefill === null ? (
            <p className="text-xs text-muted-foreground">
              The issue starts blank. Add your Tidebreak version and your system
              to it.
            </p>
          ) : (
            <>
              <p className="text-xs text-muted-foreground">
                The issue starts with only this filled in
              </p>
              <p className="mt-0.5 font-mono text-xs text-foreground">
                {prefill}
              </p>
            </>
          )}
        </div>

        {phase.step === "saved" && (
          <Notice tone="success" title="Report saved">
            Attach it to the issue that opened in your browser, and describe
            what happened.
          </Notice>
        )}
        {phase.step === "failed" && (
          // The shell words its failures as what went wrong ("Could not
          // build the diagnostics report"), so that is the title, and the
          // body is the way on.
          <Notice tone="critical" title={phase.message.replace(/\.$/, "")}>
            You can still open the issue without it.
          </Notice>
        )}

        {onAskAgent && (
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-border-subtle pt-4">
            <div className="min-w-0 flex-[1_1_16rem]">
              <p className="text-sm font-medium text-foreground">
                Ask an agent instead
              </p>
              <p className="text-sm text-muted-foreground">
                An agent in this workspace reads this session's diagnostic
                details, asks what went wrong, and files the issue with you.
              </p>
            </div>
            <Button
              type="button"
              size="sm"
              variant="outline"
              disabled={saving}
              onClick={() => {
                onOpenChange(false);
                onAskAgent();
              }}
            >
              <Bot aria-hidden="true" />
              Ask an agent
            </Button>
          </div>
        )}

        <DialogFooter>
          {phase.step === "saved" ? (
            <>
              <Button
                type="button"
                variant="ghost"
                onClick={() => void openUrl(issueUrl)}
              >
                Open the issue again
                <ExternalLink aria-hidden="true" />
              </Button>
              <Button
                ref={primary}
                type="button"
                onClick={() => onOpenChange(false)}
              >
                Done
              </Button>
            </>
          ) : (
            <>
              <Button
                type="button"
                variant="ghost"
                disabled={saving}
                onClick={() => void openUrl(issueUrl)}
              >
                Open issue without a report
              </Button>
              <Button
                ref={primary}
                type="button"
                disabled={saving}
                aria-busy={saving || undefined}
                onClick={() => void saveAndOpenIssue()}
              >
                {saving && (
                  <Spinner aria-hidden="true" className="text-current" />
                )}
                {saving ? "Saving report…" : "Save report and open issue"}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * The prefilled facts as the issue will read them, or `null` when the shell
 * could report none of them.
 */
function factsLine(facts: ProblemReportFacts | null): string | null {
  if (!facts) return "…";
  const parts = [
    facts.version ? `Tidebreak ${facts.version}` : null,
    facts.os,
    facts.arch,
  ].filter(Boolean);
  return parts.length > 0 ? parts.join(" · ") : null;
}

/**
 * The one mounted Report a problem dialog, and the Help menu items that
 * need no page: the dialog, Export Diagnostics…, Show Logs, and Release
 * Notes.
 *
 * Mounted beside the app rather than inside it, so a boot that failed or a
 * page that crashed can still report a problem, from its own button or
 * from the menu.
 */
export function ReportProblemHost() {
  const request = useReportProblem((state) => state.request);
  const open = useReportProblem((state) => state.open);
  const close = useReportProblem((state) => state.close);

  useNativeHostEvent(MENU_COMMAND_EVENT, (command) => {
    switch (command) {
      case "report-problem":
        open();
        return;
      case "export-diagnostics":
        void exportDiagnostics();
        return;
      case "show-logs":
        void revealLogsDirectory().catch((error: unknown) =>
          toast.error(
            friendlyErrorMessage(error, "Could not open the logs folder."),
          ),
        );
        return;
      case "release-notes":
        void problemReportFacts().then((facts) =>
          openInBrowser(releaseNotesUrl(facts.version)),
        );
        return;
    }
  });

  return (
    <ReportProblemDialog
      open={request !== null}
      onOpenChange={(next) => {
        if (!next) close();
      }}
      onAskAgent={request?.askAgent}
    />
  );
}

/** Help → Export Diagnostics…: the report alone, with no issue. */
async function exportDiagnostics(): Promise<void> {
  try {
    if (await saveDiagnosticsReport())
      toast.success("Diagnostics report saved.");
  } catch (error) {
    toast.error(friendlyErrorMessage(error, SAVE_FAILED));
  }
}
