import { useState, type ReactNode } from "react";
import { ChevronDown, Download, Loader2, RotateCw } from "lucide-react";

import type {
  CodeHarnessInstallSnapshot,
  HarnessDoctorEntry,
  HarnessDoctorReport,
  HarnessKind,
} from "../api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { HARNESS_ICONS } from "./HarnessPicker";
import {
  HARNESS_LABELS,
  HARNESS_TIER_LABELS,
  harnessNeedsDownload,
  isHarnessReady,
} from "./labels";

/**
 * The harness doctor, shared by the code-mode empty state and Settings.
 *
 * One row per engine, and the row leads with what the reader came for: is
 * this engine usable, and if not, what closes the gap. An engine Tidebreak
 * has not downloaded yet is a row with a Download button on it, not a fault —
 * the pins are fetched one at a time, when someone asks for one.
 *
 * Version, path, and probe stderr are diagnostics rather than answers, so
 * they sit behind each row's disclosure with the capability list. Nothing on
 * the resting surface is a `Label: value` pair.
 */

export function DoctorList({
  report,
  title,
  onRefresh,
  refreshing,
  onInstall,
  installs,
}: {
  report: HarnessDoctorReport;
  /** Section heading, where the surface has not written its own above this. */
  title?: string;
  onRefresh?: () => void;
  refreshing?: boolean;
  /** Start this engine's download. Omitted where no client can install. */
  onInstall?: (kind: HarnessKind) => void;
  /** Live install progress, keyed by engine. */
  installs?: Partial<Record<HarnessKind, CodeHarnessInstallSnapshot>>;
}) {
  const ready = report.harnesses.filter(isHarnessReady).length;
  const total = report.harnesses.length;
  return (
    <section className="flex flex-col gap-3">
      <div className="flex items-end justify-between gap-4">
        <div className="flex flex-col gap-0.5">
          {title && <h2 className="text-sm font-medium">{title}</h2>}
          {total > 0 && (
            // The verdict the rows add up to, so a reader who only needs to
            // know "can I start work" does not walk every row to find out.
            <p className="text-muted-foreground text-sm">
              {ready === 0
                ? "No engine is ready yet. Download one, or pick it when you start a workspace."
                : `${ready} of ${total} ${total === 1 ? "engine" : "engines"} ready.`}
            </p>
          )}
        </div>
        {onRefresh && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="ml-auto shrink-0"
            onClick={onRefresh}
            disabled={refreshing}
          >
            <RotateCw
              className={cn("size-3.5", refreshing && "animate-spin")}
              aria-hidden="true"
            />
            {refreshing ? "Checking…" : "Re-check"}
          </Button>
        )}
      </div>
      <div className="divide-subtle overflow-hidden rounded-lg border divide-y">
        {report.harnesses.map((entry) => (
          <DoctorRow
            key={entry.kind}
            entry={entry}
            install={installs?.[entry.kind]}
            onInstall={onInstall}
          />
        ))}
      </div>
    </section>
  );
}

/**
 * The version, without the product name a CLI repeats back.
 *
 * `claude --version` answers `2.1.234 (Claude Code)`, and this row already
 * says Claude Code two words to the left. Only a parenthetical that restates
 * the label is dropped; anything else a CLI reports is kept.
 */
function shortVersion(version: string, label: string): string {
  const trimmed = version.trim();
  const match = /^(.*?)\s*\(([^()]*)\)$/.exec(trimmed);
  if (match && match[2].toLowerCase() === label.toLowerCase()) {
    return match[1].trim() || trimmed;
  }
  return trimmed;
}

/** The badge a row leads with, and the tone it carries. */
function statusBadge(
  entry: HarnessDoctorEntry,
  install: CodeHarnessInstallSnapshot | undefined,
): {
  label: string;
  variant: "success" | "warning" | "critical" | "info" | "outline";
} {
  if (install && !install.done && !install.error) {
    return { label: "Downloading", variant: "info" };
  }
  if (install?.error) return { label: "Download failed", variant: "critical" };
  if (isHarnessReady(entry)) return { label: "Ready", variant: "success" };
  // A state, not an instruction — the instruction is the line below it.
  if (entry.found) return { label: "Signed out", variant: "warning" };
  if (harnessNeedsDownload(entry)) {
    return { label: "Not downloaded", variant: "outline" };
  }
  return { label: "Unavailable", variant: "warning" };
}

/**
 * The one line under an engine's name.
 *
 * What stands between the reader and using it, when something does. When
 * nothing does, how well this build drives the engine, which is the other
 * thing worth knowing before picking one.
 */
function subtitle(
  entry: HarnessDoctorEntry,
  install: CodeHarnessInstallSnapshot | undefined,
): string {
  if (install && !install.done && !install.error) {
    return "Downloading the pinned version. This takes a few minutes.";
  }
  if (install?.error) return install.error;
  if (entry.remediation) return entry.remediation;
  if (harnessNeedsDownload(entry)) {
    return "Downloads the first time you pick it.";
  }
  return TIER_NOTES[entry.tier];
}

/** What each adapter tier means for the reader picking an engine. */
const TIER_NOTES: Record<HarnessDoctorEntry["tier"], string> = {
  reference: "Reference adapter — the engine this build is built around.",
  secondary: "Secondary adapter — well covered, a step behind the reference.",
  tertiary: "Tertiary adapter — the common paths are covered.",
  best_effort: "Best-effort adapter — expect gaps in what it can do.",
};

function DoctorRow({
  entry,
  install,
  onInstall,
}: {
  entry: HarnessDoctorEntry;
  install: CodeHarnessInstallSnapshot | undefined;
  onInstall?: (kind: HarnessKind) => void;
}) {
  const [open, setOpen] = useState(false);
  const Icon = HARNESS_ICONS[entry.kind];
  const badge = statusBadge(entry, install);
  const downloading = Boolean(install && !install.done && !install.error);
  const failed = Boolean(install?.error);
  const canDownload =
    Boolean(onInstall) && !entry.found && entry.installable && !downloading;
  const detailId = `harness-detail-${entry.kind}`;

  return (
    <div className="bg-background flex flex-col">
      {/* One grid so every row is the same height whatever it carries: the
          name line, one note, and the controls all sit on fixed tracks. */}
      <div className="flex items-center gap-3 px-4 py-3">
        <Icon className="text-foreground size-5 shrink-0" />
        <div className="flex min-w-0 flex-1 flex-col gap-0.5">
          <div className="flex min-w-0 items-center gap-2">
            <span className="text-md truncate font-medium">
              {HARNESS_LABELS[entry.kind]}
            </span>
            {entry.version && (
              <span className="text-muted-foreground truncate font-mono text-xs">
                {shortVersion(entry.version, HARNESS_LABELS[entry.kind])}
              </span>
            )}
            <Badge variant={badge.variant} size="sm" className="shrink-0">
              {downloading && (
                <Loader2 className="size-3 animate-spin" aria-hidden="true" />
              )}
              {badge.label}
            </Badge>
          </div>
          <p
            className={cn(
              "truncate text-xs",
              // A failed install's only content is npm's own words, so it
              // renders in the machine voice rather than as prose.
              failed
                ? "text-critical-foreground font-mono"
                : "text-muted-foreground",
            )}
          >
            {subtitle(entry, install)}
          </p>
        </div>
        {canDownload && onInstall && (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="shrink-0"
            onClick={() => onInstall(entry.kind)}
          >
            <Download className="size-3.5" aria-hidden="true" />
            {failed ? "Retry" : "Download"}
          </Button>
        )}
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          className="text-muted-foreground shrink-0"
          onClick={() => setOpen((current) => !current)}
          aria-expanded={open}
          aria-controls={detailId}
          aria-label={`Details for ${HARNESS_LABELS[entry.kind]}`}
        >
          <ChevronDown
            className={cn("size-4 transition-transform", open && "rotate-180")}
            aria-hidden="true"
          />
        </Button>
      </div>
      {open && (
        <dl
          id={detailId}
          className="border-subtle text-muted-foreground grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 border-t px-4 py-3 pl-12 text-sm"
        >
          <Detail label="Tier">{HARNESS_TIER_LABELS[entry.tier]}</Detail>
          <Detail label="Signed in">
            {entry.authenticated === undefined
              ? "not observed"
              : entry.authenticated
                ? "yes"
                : "no"}
          </Detail>
          {entry.path && (
            <Detail label="Path">
              <span className="font-mono break-all">{entry.path}</span>
            </Detail>
          )}
          <Detail label="Supports">{capsSummary(entry)}</Detail>
          {entry.unrecognized_event_count > 0 && (
            <Detail label="Protocol gaps (history)">
              {`${entry.unrecognized_event_count} ${entry.unrecognized_event_count === 1 ? "event" : "events"} across saved sessions`}
            </Detail>
          )}
          {entry.stderr && (
            <Detail label="Probe output">
              <span className="font-mono break-all">{entry.stderr}</span>
            </Detail>
          )}
        </dl>
      )}
    </div>
  );
}

function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="text-foreground font-medium whitespace-nowrap">
        {label}:
      </dt>
      <dd className="min-w-0">{children}</dd>
    </>
  );
}

function capsSummary(entry: HarnessDoctorEntry): string {
  const supported = Object.entries(entry.caps)
    .filter(([, level]) => level === "supported")
    .map(([name]) => name.replaceAll("_", " "));
  return supported.length > 0
    ? supported.join(", ")
    : "none stated as supported";
}
