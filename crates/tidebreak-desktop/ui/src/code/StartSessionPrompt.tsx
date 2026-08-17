import { useState } from "react";

import type {
  CodePermissionMode,
  HarnessDoctorEntry,
  HarnessKind,
} from "../api/types";
import { Button } from "@/components/ui/button";
import { PermissionModePicker } from "./CodeComposer";
import { HarnessPicker } from "./HarnessPicker";
import {
  autoIsUnsupervised,
  createPermissionModes,
  defaultCreatePermissionMode,
  harnessUnusableReason,
  ALLOW_ALL_NOTE,
  UNSUPERVISED_AUTO_NOTE,
} from "./labels";

/**
 * Create-time harness + permission mode for a workspace with no session.
 *
 * The harness dropdown defaults to the first ready engine; the mode list and
 * default follow the selected engine's own capability flags, so start always
 * posts a mode that engine can honor. Unsupervised Auto says so before the
 * session exists (decision 0038).
 */
export function StartSessionPrompt({
  harnesses,
  starting,
  selectedMode,
  onSelectMode,
  onStart,
}: {
  harnesses: HarnessDoctorEntry[];
  starting: boolean;
  selectedMode: CodePermissionMode | null;
  onSelectMode: (mode: CodePermissionMode) => void;
  onStart: (harness: HarnessKind, mode: CodePermissionMode) => void;
}) {
  const [picked, setPicked] = useState<HarnessKind | null>(null);
  const ready = harnesses.filter((entry) => !harnessUnusableReason(entry));
  const selected =
    harnesses.find((entry) => entry.kind === picked && !harnessUnusableReason(entry)) ??
    ready[0];
  const availableModes = selected ? createPermissionModes(selected.caps) : [];
  const mode: CodePermissionMode =
    selectedMode && availableModes.includes(selectedMode)
      ? selectedMode
      : selected
        ? defaultCreatePermissionMode(selected.caps)
        : "plan";

  return (
    <div className="flex flex-col gap-3 px-4 py-6">
      <p className="text-sm">Start a session on this workspace.</p>
      <HarnessPicker
        harnesses={harnesses}
        value={selected?.kind ?? null}
        disabled={starting}
        onChange={setPicked}
      />
      <PermissionModePicker
        value={mode}
        availableModes={availableModes}
        onChange={onSelectMode}
      />
      {mode === "auto" && selected && autoIsUnsupervised(selected.caps) && (
        <p className="text-warning-foreground text-xs">{UNSUPERVISED_AUTO_NOTE}</p>
      )}
      {mode === "allow" && (
        <p className="text-warning-foreground text-xs">{ALLOW_ALL_NOTE}</p>
      )}
      <Button
        type="button"
        className="w-fit"
        disabled={starting || !selected}
        onClick={() => selected && onStart(selected.kind, mode)}
      >
        {starting ? "Starting…" : "Start session"}
      </Button>
    </div>
  );
}
