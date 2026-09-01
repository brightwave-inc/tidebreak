import type { ComponentType } from "react";
import { useNavigate } from "@tanstack/react-router";

import type { HarnessDoctorEntry, HarnessKind } from "../api/types";
import {
  ClaudeIcon,
  OpenAIIcon,
  OpenCodeIcon,
  XaiIcon,
} from "../ProviderIcons";
import { Logomark } from "../Logomark";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  HARNESS_LABELS,
  harnessNeedsDownload,
  harnessUnusableReason,
} from "./labels";

/** What picking a not-yet-downloaded engine does. */
const DOWNLOAD_NOTE = "Downloads on first use";

export const HARNESS_ICONS: Record<
  HarnessKind,
  ComponentType<{ className?: string }>
> = {
  claude_code: ClaudeIcon,
  codex: OpenAIIcon,
  opencode: OpenCodeIcon,
  grok: XaiIcon,
  internal: Logomark,
};

/**
 * Branded harness dropdown: logo and product name per row.
 *
 * Ready rows are selectable and carry nothing but the name — a vendor gloss
 * tells a reader picking an engine nothing they do not already know. A row
 * this machine has not downloaded yet is still selectable and says so, since
 * choosing it is what starts the download. Only a row that cannot be fixed by
 * waiting is disabled, with the one reason it cannot be chosen; a quiet link
 * to the doctor appears under the control while any row is unusable. Versions
 * and capability flags stay off this surface.
 */
export function HarnessPicker({
  harnesses,
  value,
  onChange,
  disabled,
  variant = "field",
}: {
  harnesses: HarnessDoctorEntry[];
  value: HarnessKind | null;
  onChange: (kind: HarnessKind) => void;
  disabled?: boolean;
  /** Field fills a form row; composer sits beside model and mode controls. */
  variant?: "field" | "composer";
}) {
  const navigate = useNavigate();
  const harnessesPath: string = "/settings/coding-harnesses";
  const selected = harnesses.find((entry) => entry.kind === value);
  const SelectedIcon = selected ? HARNESS_ICONS[selected.kind] : null;
  const anyUnusable = harnesses.some((entry) => harnessUnusableReason(entry));

  return (
    <div
      className={
        variant === "composer"
          ? "flex min-w-0 items-center gap-1"
          : "flex flex-col gap-1"
      }
    >
      <Select
        value={value ?? undefined}
        onValueChange={(next) => onChange(next as HarnessKind)}
        disabled={disabled || harnesses.length === 0}
      >
        <SelectTrigger
          aria-label="Harness"
          className={
            variant === "composer"
              ? "h-8 w-auto max-w-44 min-w-0 shrink-0 gap-2 border-transparent px-2 hover:bg-accent hover:text-accent-foreground"
              : undefined
          }
        >
          <SelectValue placeholder="No harness detected">
            {selected && SelectedIcon && (
              <span className="flex items-center gap-2">
                <SelectedIcon className="size-4 shrink-0" />
                <span className="truncate">
                  {HARNESS_LABELS[selected.kind]}
                </span>
              </span>
            )}
          </SelectValue>
        </SelectTrigger>
        <SelectContent scrollButtons={false}>
          {harnesses.map((entry) => {
            const reason = harnessUnusableReason(entry);
            const note =
              reason ?? (harnessNeedsDownload(entry) ? DOWNLOAD_NOTE : null);
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
                    {note && (
                      <span className="text-muted-foreground truncate text-xs">
                        {note}
                      </span>
                    )}
                  </span>
                </span>
              </SelectItem>
            );
          })}
        </SelectContent>
      </Select>
      {anyUnusable && variant === "field" && (
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
