import {
  useEffect,
  useId,
  useRef,
  useState,
  type FormEvent,
} from "react";
import {
  MonitorSmartphone,
  Smartphone,
  Tablet,
  Maximize,
  SlidersHorizontal,
} from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { FOCUS_RING_TIGHT, HOVER_TINT } from "../interactive";
import {
  type BrowserViewport,
  type BrowserViewportPreset,
  clampCustomWidth,
  MAX_CUSTOM_WIDTH,
  MIN_CUSTOM_WIDTH,
  viewportLabel,
  VIEWPORT_PRESET_LABELS,
} from "./browserViewport";

const PRESET_ORDER: BrowserViewportPreset[] = [
  "fit",
  "desktop",
  "tablet",
  "mobile",
];

const PRESET_ICONS: Record<
  Exclude<BrowserViewportPreset, "custom">,
  typeof MonitorSmartphone
> = {
  fit: Maximize,
  desktop: MonitorSmartphone,
  tablet: Tablet,
  mobile: Smartphone,
};

const PRESET_ICON_FOR = (preset: BrowserViewportPreset) =>
  preset === "custom" ? SlidersHorizontal : PRESET_ICONS[preset];

export type BrowserViewportControlProps = {
  viewport: BrowserViewport;
  /** Actual rendered width in CSS px, for legibility without a second control. */
  renderedWidth: number | null;
  onViewportChange: (viewport: BrowserViewport) => void;
  disabled?: boolean;
};

/**
 * Compact viewport selector for the browser toolbar.
 *
 * A single popover button carries Fit + desktop/tablet/mobile presets and a
 * bounded custom-width field. The trigger label shows the active preset and
 * its effective width so the user never needs a second readout. Custom-width
 * validation is inline and non-blocking. Uses a popover (not a dropdown menu)
 * so the custom-width form stays interactive without auto-closing on submit.
 */
export function BrowserViewportControl({
  viewport,
  renderedWidth,
  onViewportChange,
  disabled = false,
}: BrowserViewportControlProps) {
  const [open, setOpen] = useState(false);
  const triggerLabelId = useId();
  const label = viewportLabel(viewport);
  const widthText =
    renderedWidth !== null && renderedWidth > 0
      ? `${Math.round(renderedWidth)}px`
      : null;
  const triggerAriaLabel = `Viewport: ${label}${
    widthText ? `, rendered at ${widthText}` : ""
  }`;

  function selectPreset(preset: BrowserViewportPreset) {
    onViewportChange({ ...viewport, preset });
  }

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <WithTooltip label="Viewport size">
        <PopoverTrigger asChild>
          <Button
            type="button"
            variant="ghost"
            size="xs"
            className="h-7 gap-1.5 px-2 text-[10px] font-medium"
            disabled={disabled}
            aria-label={triggerAriaLabel}
          >
            <SlidersHorizontal className="size-3.5 shrink-0 text-muted-foreground" />
            <span id={triggerLabelId} className="max-w-24 truncate">
              {label}
            </span>
            {widthText && (
              <span className="shrink-0 text-muted-foreground/70">
                {widthText}
              </span>
            )}
          </Button>
        </PopoverTrigger>
      </WithTooltip>
      <PopoverContent
        align="end"
        className="w-60 p-2"
        onOpenAutoFocus={(event) => event.preventDefault()}
      >
        <ViewportPresetList
          viewport={viewport}
          onSelectPreset={selectPreset}
        />
        <div className="my-1.5 h-px bg-border-subtle" />
        <CustomWidthField
          viewport={viewport}
          onViewportChange={onViewportChange}
          controlGroupId={triggerLabelId}
        />
      </PopoverContent>
    </Popover>
  );
}

function ViewportPresetList({
  viewport,
  onSelectPreset,
}: {
  viewport: BrowserViewport;
  onSelectPreset: (preset: BrowserViewportPreset) => void;
}) {
  return (
    <div role="radiogroup" aria-label="Viewport presets">
      {PRESET_ORDER.map((preset) => {
        const Icon = PRESET_ICON_FOR(preset);
        const active = viewport.preset === preset;
        return (
          <button
            key={preset}
            type="button"
            role="radio"
            aria-checked={active}
            aria-label={VIEWPORT_PRESET_LABELS[preset]}
            tabIndex={active ? 0 : -1}
            className={cn(
              "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-xs font-medium",
              FOCUS_RING_TIGHT,
              HOVER_TINT,
              active
                ? "bg-accent text-accent-foreground"
                : "text-muted-foreground hover:text-foreground",
            )}
            onClick={() => onSelectPreset(preset)}
          >
            <Icon className="size-3.5 shrink-0 text-muted-foreground" />
            <span className="flex-1 truncate">{VIEWPORT_PRESET_LABELS[preset]}</span>
            {active && (
              <span
                aria-hidden
                className="size-1.5 rounded-full bg-primary"
              />
            )}
          </button>
        );
      })}
    </div>
  );
}

function CustomWidthField({
  viewport,
  onViewportChange,
  controlGroupId,
}: {
  viewport: BrowserViewport;
  onViewportChange: (viewport: BrowserViewport) => void;
  controlGroupId: string;
}) {
  const inputRef = useRef<HTMLInputElement | null>(null);
  const errorId = useId();
  const [draft, setDraft] = useState(String(viewport.customWidth));
  const [error, setError] = useState<string | null>(null);

  // Keep the draft in sync when the viewport changes externally (preset switch).
  useEffect(() => {
    setDraft(String(viewport.customWidth));
    setError(null);
  }, [viewport.customWidth]);

  function commit(event: FormEvent) {
    event.preventDefault();
    const parsed = Number.parseInt(draft, 10);
    if (!Number.isFinite(parsed)) {
      setError("Enter a number");
      return;
    }
    const clamped = clampCustomWidth(parsed);
    if (parsed < MIN_CUSTOM_WIDTH || parsed > MAX_CUSTOM_WIDTH) {
      setError(
        `Width must be between ${MIN_CUSTOM_WIDTH} and ${MAX_CUSTOM_WIDTH}`,
      );
    } else {
      setError(null);
    }
    onViewportChange({ preset: "custom", customWidth: clamped });
    setDraft(String(clamped));
    inputRef.current?.focus();
  }

  return (
    <form
      onSubmit={commit}
      className="px-1 py-0.5"
      aria-labelledby={`${controlGroupId}-custom-label`}
    >
      <div className="flex items-center gap-2">
        <label
          id={`${controlGroupId}-custom-label`}
          htmlFor={`${controlGroupId}-custom-input`}
          className="flex items-center gap-1.5 text-[11px] font-medium text-muted-foreground"
        >
          <SlidersHorizontal className="size-3" />
          Custom width
        </label>
        <input
          ref={inputRef}
          id={`${controlGroupId}-custom-input`}
          type="number"
          inputMode="numeric"
          min={MIN_CUSTOM_WIDTH}
          max={MAX_CUSTOM_WIDTH}
          step={8}
          value={draft}
          aria-label="Custom viewport width in pixels"
          aria-invalid={Boolean(error)}
          aria-describedby={error ? errorId : undefined}
          className={cn(
            "h-7 w-20 rounded-md border bg-background px-2 text-xs font-medium tabular-nums outline-none",
            "focus:border-ring focus:ring-2 focus:ring-ring/20",
            error && "border-critical-border ring-1 ring-critical/10",
          )}
          onChange={(event) => {
            setDraft(event.target.value);
            if (error) setError(null);
          }}
          onFocus={(event) => event.currentTarget.select()}
        />
        <span className="text-[10px] text-muted-foreground/70">px</span>
      </div>
      {error && (
        <p
          id={errorId}
          role="alert"
          className="mt-1 text-[10px] text-critical-foreground"
        >
          {error}
        </p>
      )}
      <p className="mt-1 text-[10px] text-muted-foreground/60">
        {MIN_CUSTOM_WIDTH}–{MAX_CUSTOM_WIDTH} px
      </p>
    </form>
  );
}
