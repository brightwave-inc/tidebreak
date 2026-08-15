import type { HarnessDoctorEntry, HarnessDoctorReport } from "../api/types";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  HARNESS_LABELS,
  HARNESS_TIER_LABELS,
  isHarnessReady,
} from "./labels";

/**
 * The harness doctor, shared by the code-mode empty state and Settings.
 *
 * Found / version / auth / remediation are the facts a reader can act on.
 * Capability flags stay a one-line summary so the page does not become a
 * matrix of every adapter's self-report.
 */

export function DoctorList({
  report,
  onRefresh,
  refreshing,
}: {
  report: HarnessDoctorReport;
  onRefresh?: () => void;
  refreshing?: boolean;
}) {
  return (
    <div className="flex flex-col gap-3">
      {onRefresh && (
        <div className="flex justify-end">
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={onRefresh}
            disabled={refreshing}
          >
            {refreshing ? "Refreshing…" : "Refresh"}
          </Button>
        </div>
      )}
      {report.harnesses.map((entry) => (
        <DoctorCard key={entry.kind} entry={entry} />
      ))}
    </div>
  );
}

function DoctorCard({ entry }: { entry: HarnessDoctorEntry }) {
  const ready = isHarnessReady(entry);
  return (
    <Card className="gap-3 p-4">
      <CardHeader className="justify-between">
        <CardTitle className="text-base">{HARNESS_LABELS[entry.kind]}</CardTitle>
        <Badge variant={ready ? "success" : "warning"} size="sm">
          {ready ? "Ready" : entry.found ? "Sign in" : "Not found"}
        </Badge>
      </CardHeader>
      <CardContent className="gap-1 text-sm">
        <Row label="Found" value={entry.found ? "yes" : "no"} />
        {entry.path && <Row label="Path" value={entry.path} />}
        {entry.version && <Row label="Version" value={entry.version} />}
        <Row label="Tier" value={HARNESS_TIER_LABELS[entry.tier]} />
        <Row label="Capabilities" value={capsSummary(entry)} />
        <Row
          label="Authenticated"
          value={
            entry.authenticated === undefined
              ? "unknown"
              : entry.authenticated
                ? "yes"
                : "no"
          }
        />
        {entry.unrecognized_event_count > 0 && (
          <Row
            label="Unrecognized events"
            value={String(entry.unrecognized_event_count)}
          />
        )}
        {entry.remediation && (
          <p className="text-warning-foreground mt-2">{entry.remediation}</p>
        )}
      </CardContent>
    </Card>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <p className="text-muted-foreground">
      <span className="text-foreground font-medium">{label}: </span>
      {value}
    </p>
  );
}

function capsSummary(entry: HarnessDoctorEntry): string {
  const supported = Object.entries(entry.caps)
    .filter(([, level]) => level === "supported")
    .map(([name]) => name.replaceAll("_", " "));
  return supported.length > 0 ? supported.join(", ") : "none stated as supported";
}
