import type { ComponentType } from "react";
import { Columns2, Pilcrow } from "lucide-react";

import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "../interactive";
import { useDiffPreferences } from "./diffPreferences";

/**
 * The diff's two view switches: side by side, and hiding whitespace changes.
 * The layout is the reader's standing preference; whitespace belongs to the
 * one diff it was hidden in.
 */
export function DiffViewOptions({
  ignoreWhitespace,
  onIgnoreWhitespaceChange,
}: {
  ignoreWhitespace: boolean;
  onIgnoreWhitespaceChange: (ignore: boolean) => void;
}) {
  const layout = useDiffPreferences((state) => state.layout);
  const setLayout = useDiffPreferences((state) => state.setLayout);
  return (
    <div
      role="group"
      aria-label="Diff view"
      className="flex items-center gap-0.5"
    >
      <ViewToggle
        label="Split view"
        hint="Side by side, when the pane is wide enough"
        icon={Columns2}
        pressed={layout === "split"}
        onPressedChange={(pressed) => setLayout(pressed ? "split" : "unified")}
      />
      <ViewToggle
        label="Hide whitespace changes"
        hint="W"
        icon={Pilcrow}
        pressed={ignoreWhitespace}
        onPressedChange={onIgnoreWhitespaceChange}
      />
    </div>
  );
}

function ViewToggle({
  label,
  hint,
  icon: Icon,
  pressed,
  onPressedChange,
}: {
  label: string;
  hint: string;
  icon: ComponentType<{ className?: string; "aria-hidden"?: boolean }>;
  pressed: boolean;
  onPressedChange: (pressed: boolean) => void;
}) {
  return (
    <WithTooltip
      label={
        <>
          {label}
          <span className="text-primary-foreground mt-0.5 block text-xs font-normal">
            {hint}
          </span>
        </>
      }
    >
      <button
        type="button"
        aria-label={label}
        aria-pressed={pressed}
        className={cn(
          "grid size-6 shrink-0 cursor-pointer place-items-center rounded-md",
          pressed
            ? "bg-muted text-foreground"
            : "text-muted-foreground hover:bg-muted hover:text-foreground",
          FOCUS_RING_TIGHT,
          HOVER_TINT,
        )}
        onClick={() => onPressedChange(!pressed)}
      >
        <Icon className="size-3.5" aria-hidden />
      </button>
    </WithTooltip>
  );
}
