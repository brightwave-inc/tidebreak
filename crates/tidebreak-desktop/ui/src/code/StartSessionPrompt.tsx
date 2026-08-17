import type {
  CodePermissionMode,
  HarnessDoctorEntry,
  HarnessKind,
} from "../api/types";
import { PermissionModePicker } from "./CodeComposer";
import { HarnessPicker } from "./HarnessPicker";
import {
  createPermissionModes,
  defaultCreatePermissionMode,
  harnessUnusableReason,
} from "./labels";

/**
 * Create-time harness + permission mode. Defaults to Ask when any ready
 * harness reports structured approvals, otherwise Plan, so create always
 * posts a mode the selected engine can honor.
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
  const usable = harnesses.filter((entry) => !harnessUnusableReason(entry));
  const anyAsk = usable.some((entry) =>
    createPermissionModes(entry.caps.structured_approvals).includes("ask"),
  );
  const defaultMode: CodePermissionMode = anyAsk ? "ask" : "plan";
  const mode = selectedMode ?? defaultMode;
  const availableModes: CodePermissionMode[] = anyAsk
    ? ["plan", "ask", "auto"]
    : ["plan"];

  return (
    <div className="flex flex-col gap-3 px-4 py-6">
      <p className="text-sm">Start a session on this workspace.</p>
      <PermissionModePicker
        value={mode}
        availableModes={availableModes}
        onChange={onSelectMode}
      />
      <HarnessPicker
        harnesses={harnesses}
        value={null}
        disabled={starting}
        onChange={(kind) => {
          const entry = harnesses.find((item) => item.kind === kind);
          if (!entry) return;
          onStart(entry.kind, modeForHarness(entry, mode));
        }}
      />
    </div>
  );
}

function modeForHarness(
  entry: HarnessDoctorEntry,
  selected: CodePermissionMode,
): CodePermissionMode {
  const available = createPermissionModes(entry.caps.structured_approvals);
  if (available.includes(selected)) return selected;
  return defaultCreatePermissionMode(entry.caps.structured_approvals);
}
