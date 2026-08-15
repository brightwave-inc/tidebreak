import type {
  CodePermissionMode,
  HarnessDoctorEntry,
  HarnessKind,
} from "../api/types";
import { Button } from "@/components/ui/button";
import { PermissionModePicker } from "./CodeComposer";
import {
  createPermissionModes,
  defaultCreatePermissionMode,
  HARNESS_LABELS,
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
  const anyAsk = harnesses.some((entry) =>
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
      <div className="flex flex-wrap gap-2">
        {harnesses.map((entry) => {
          const posted = modeForHarness(entry, mode);
          return (
            <Button
              key={entry.kind}
              type="button"
              size="sm"
              disabled={starting}
              onClick={() => onStart(entry.kind, posted)}
            >
              {HARNESS_LABELS[entry.kind]}
            </Button>
          );
        })}
      </div>
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
