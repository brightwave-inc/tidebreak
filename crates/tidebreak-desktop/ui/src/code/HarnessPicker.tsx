import type { ComponentType, KeyboardEvent } from "react";
import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";

import type { HarnessDoctorEntry, HarnessKind } from "../api/types";
import {
  ClaudeIcon,
  OpenAIIcon,
  OpenCodeIcon,
  XaiIcon,
} from "../ProviderIcons";
import { cn } from "@/lib/utils";
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
 * Branded harness picker: logo, product name, one-line subtitle.
 *
 * Ready rows are selectable. Unusable rows are dimmed with a short reason
 * and a quiet link to the doctor. Versions and capability flags stay off
 * this surface.
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
  const [active, setActive] = useState(() =>
    Math.max(
      0,
      harnesses.findIndex((entry) => {
        if (value) return entry.kind === value;
        return !harnessUnusableReason(entry);
      }),
    ),
  );

  useEffect(() => {
    const next = harnesses.findIndex((entry) => {
      if (value) return entry.kind === value;
      return !harnessUnusableReason(entry);
    });
    if (next >= 0) setActive(next);
  }, [harnesses, value]);

  function move(delta: number) {
    if (harnesses.length === 0) return;
    setActive((current) => {
      const next = (current + delta + harnesses.length) % harnesses.length;
      return next;
    });
  }

  function pick(index: number) {
    const entry = harnesses[index];
    if (!entry || harnessUnusableReason(entry) || disabled) return;
    onChange(entry.kind);
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      move(1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      move(-1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      pick(active);
    }
  }

  return (
    <div
      role="listbox"
      aria-label="Harness"
      tabIndex={disabled ? -1 : 0}
      className="flex flex-col gap-0.5 rounded-md border border-border p-1"
      onKeyDown={onKeyDown}
    >
      {harnesses.map((entry, index) => {
        const reason = harnessUnusableReason(entry);
        const selected = value === entry.kind;
        const Icon = HARNESS_ICONS[entry.kind];
        return (
          <button
            key={entry.kind}
            type="button"
            role="option"
            aria-selected={selected}
            aria-disabled={Boolean(reason) || undefined}
            disabled={disabled || Boolean(reason)}
            className={cn(
              "flex w-full items-start gap-2.5 rounded-sm px-2 py-1.5 text-left text-sm",
              index === active && "bg-accent text-accent-foreground",
              reason && "opacity-50",
              selected && !reason && "ring-1 ring-border",
            )}
            onMouseEnter={() => setActive(index)}
            onClick={() => pick(index)}
          >
            <Icon className="mt-0.5 size-4 shrink-0" />
            <span className="flex min-w-0 flex-col">
              <span className="truncate font-medium">
                {HARNESS_LABELS[entry.kind]}
              </span>
              <span className="truncate text-xs text-muted-foreground">
                {reason ?? HARNESS_SUBTITLES[entry.kind]}
              </span>
              {reason && (
                <span
                  role="link"
                  tabIndex={0}
                  className="text-muted-foreground mt-0.5 w-fit cursor-pointer text-xs underline-offset-2 hover:underline"
                  onClick={(event) => {
                    event.stopPropagation();
                    void navigate({ to: harnessesPath });
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      event.stopPropagation();
                      void navigate({ to: harnessesPath });
                    }
                  }}
                >
                  Coding harnesses
                </span>
              )}
            </span>
          </button>
        );
      })}
    </div>
  );
}
