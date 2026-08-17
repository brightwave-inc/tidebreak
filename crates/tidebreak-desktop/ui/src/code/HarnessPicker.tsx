import type { ComponentType } from "react";
import { useNavigate } from "@tanstack/react-router";

import type { HarnessDoctorEntry, HarnessKind } from "../api/types";
import {
  ClaudeIcon,
  OpenAIIcon,
  OpenCodeIcon,
  XaiIcon,
} from "../ProviderIcons";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  HARNESS_LABELS,
  HARNESS_SUBTITLES,
  harnessUnusableReason,
} from "./labels";

const HARNESS_ICONS: Record<
  HarnessKind,
  ComponentType<{ className?: string }>
> = {
  claude_code: ClaudeIcon,
  codex: OpenAIIcon,
  opencode: OpenCodeIcon,
  grok: XaiIcon,
};

/**
 * Branded harness dropdown: logo, product name, one-line subtitle per row.
 *
 * Ready rows are selectable. Unusable rows stay listed, disabled, with a
 * short reason in place of the subtitle; a quiet link to the doctor appears
 * under the control while any row is unusable. Versions and capability
 * flags stay off this surface.
 */
export function HarnessPicker({
  harnesses,
  value,
  onChange,
  disabled,
}: {
  harnesses: HarnessDoctorEntry[];
  value: HarnessKind | null;
  onChange: (kind: HarnessKind) => void;
  disabled?: boolean;
}) {
  const navigate = useNavigate();
  const harnessesPath: string = "/settings/coding-harnesses";
  const selected = harnesses.find((entry) => entry.kind === value);
  const SelectedIcon = selected ? HARNESS_ICONS[selected.kind] : null;
  const anyUnusable = harnesses.some((entry) => harnessUnusableReason(entry));

  return (
    <div className="flex flex-col gap-1">
      <Select
        value={value ?? undefined}
        onValueChange={(next) => onChange(next as HarnessKind)}
        disabled={disabled || harnesses.length === 0}
      >
        <SelectTrigger aria-label="Harness">
          <SelectValue placeholder="No harness detected">
            {selected && SelectedIcon && (
              <span className="flex items-center gap-2">
                <SelectedIcon className="size-4 shrink-0" />
                <span className="truncate">{HARNESS_LABELS[selected.kind]}</span>
              </span>
            )}
          </SelectValue>
        </SelectTrigger>
        <SelectContent scrollButtons={false}>
          {harnesses.map((entry) => {
            const reason = harnessUnusableReason(entry);
            const Icon = HARNESS_ICONS[entry.kind];
            return (
              <SelectItem
                key={entry.kind}
                value={entry.kind}
                disabled={Boolean(reason)}
              >
                <span className="flex items-center gap-2.5">
                  <Icon className="size-4 shrink-0" />
                  <span className="flex min-w-0 flex-col text-left">
                    <span className="truncate font-medium">
                      {HARNESS_LABELS[entry.kind]}
                    </span>
                    <span className="text-muted-foreground truncate text-xs">
                      {reason ?? HARNESS_SUBTITLES[entry.kind]}
                    </span>
                  </span>
                </span>
              </SelectItem>
            );
          })}
        </SelectContent>
      </Select>
      {anyUnusable && (
        <button
          type="button"
          className="text-muted-foreground w-fit cursor-pointer text-xs underline-offset-2 hover:underline"
          onClick={() => void navigate({ to: harnessesPath })}
        >
          Coding harnesses
        </button>
      )}
    </div>
  );
}
