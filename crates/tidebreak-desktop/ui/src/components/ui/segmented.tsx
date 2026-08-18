import {
  useRef,
  type KeyboardEvent,
  type ReactNode,
} from "react";

import { cn } from "@/lib/utils";

export type SegmentedOption<T extends string> = {
  value: T;
  label: string;
  icon?: ReactNode;
};

/**
 * Two-or-more equal segments with radiogroup semantics.
 *
 * The active segment is filled. Arrow keys, Home, and End move the selection
 * and the roving tabindex, matching a native radio group.
 */
export function SegmentedControl<T extends string>({
  value,
  onValueChange,
  options,
  "aria-label": ariaLabel,
  className,
}: {
  value: T;
  onValueChange: (value: T) => void;
  options: readonly SegmentedOption<T>[];
  "aria-label": string;
  className?: string;
}) {
  const refs = useRef<Array<HTMLButtonElement | null>>([]);

  function selectAt(index: number) {
    const option = options[index];
    if (!option) return;
    onValueChange(option.value);
    refs.current[index]?.focus();
  }

  function onKeyDown(event: KeyboardEvent<HTMLButtonElement>, index: number) {
    const last = options.length - 1;
    if (event.key === "ArrowRight" || event.key === "ArrowDown") {
      event.preventDefault();
      selectAt(index === last ? 0 : index + 1);
      return;
    }
    if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
      event.preventDefault();
      selectAt(index === 0 ? last : index - 1);
      return;
    }
    if (event.key === "Home") {
      event.preventDefault();
      selectAt(0);
      return;
    }
    if (event.key === "End") {
      event.preventDefault();
      selectAt(last);
    }
  }

  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      className={cn(
        "grid w-full gap-0.5 rounded-md bg-muted/70 p-0.5",
        className,
      )}
      style={{ gridTemplateColumns: `repeat(${options.length}, minmax(0, 1fr))` }}
    >
      {options.map((option, index) => {
        const checked = option.value === value;
        return (
          <button
            key={option.value}
            ref={(node) => {
              refs.current[index] = node;
            }}
            type="button"
            role="radio"
            aria-checked={checked}
            aria-label={option.label}
            tabIndex={checked ? 0 : -1}
            data-state={checked ? "on" : "off"}
            className={cn(
              "inline-flex min-w-0 cursor-pointer items-center justify-center gap-1.5 rounded-[5px] px-2 py-1 text-[12px] font-medium",
              "ring-offset-background focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:outline-none",
              "motion-safe:transition-colors motion-safe:duration-150 motion-safe:ease-out",
              checked
                ? "bg-background text-foreground"
                : "text-muted-foreground hover:text-foreground",
            )}
            onClick={() => onValueChange(option.value)}
            onKeyDown={(event) => onKeyDown(event, index)}
          >
            {option.icon}
            <span className="truncate">{option.label}</span>
          </button>
        );
      })}
    </div>
  );
}
