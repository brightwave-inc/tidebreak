import { useState } from "react";
import { Atom, Check, ChevronDown, Gauge, Info, Quote } from "lucide-react";
import type {
  CitationFormat,
  ModelInfo,
  ModelSelectionKey,
  ProviderKind,
  ReasoningEffort,
} from "./api";
import { CITATION_FORMAT_LABELS, CITATION_FORMAT_OPTIONS } from "./CitationFormats";
import { canonicalModelSelection, modelForSelection } from "./ModelSelection";
import { Button } from "@/components/ui/button";
import { ProviderIcon } from "./ProviderIcons";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Switch } from "@/components/ui/switch";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

/**
 * The order the vendors are listed in, ahead of any the catalog carries that
 * this build does not know about. Fixed rather than first-seen so the list does
 * not reshuffle when a provider is configured or a custom endpoint gains a
 * model — muscle memory is worth more here than catalog order.
 */
const PROVIDER_ORDER: readonly ProviderKind[] = ["anthropic", "openai", "gemini"];

/** Catalog rows by provider, in {@link PROVIDER_ORDER}, then the rest as found. */
function groupByProvider(
  models: readonly ModelInfo[],
): { provider: ProviderKind; models: ModelInfo[] }[] {
  const byProvider = new Map<ProviderKind, ModelInfo[]>();
  for (const model of models) {
    const list = byProvider.get(model.provider);
    if (list) list.push(model);
    else byProvider.set(model.provider, [model]);
  }

  const groups: { provider: ProviderKind; models: ModelInfo[] }[] = [];
  for (const provider of PROVIDER_ORDER) {
    const found = byProvider.get(provider);
    if (found) {
      groups.push({ provider, models: found });
      byProvider.delete(provider);
    }
  }
  for (const [provider, found] of byProvider) {
    groups.push({ provider, models: found });
  }
  return groups;
}

/**
 * The groups the composer picker actually shows: providers with at least one
 * model that can run, plus — whatever its availability — the group holding the
 * current selection, so an active override never vanishes from the menu that
 * set it. A provider with nothing usable renders as no group at all rather
 * than a run of disabled rows; discovery of the full registry belongs to the
 * settings Models surface, not here.
 */
export function visibleModelGroups(
  models: readonly ModelInfo[],
  selectedKey: string | null,
): { provider: ProviderKind; models: ModelInfo[] }[] {
  return groupByProvider(models).filter(
    (group) =>
      group.models.some((model) => model.available) ||
      (selectedKey !== null &&
        group.models.some((model) => model.key === selectedKey)),
  );
}

/**
 * The model turning the Default switch off should land on: the first
 * available row *as the menu renders them*, so the selection visibly lands
 * at the top of the list rather than on whatever the catalog happened to
 * order first. `null` when nothing can run.
 */
export function firstAvailableModel(
  models: readonly ModelInfo[],
  selectedKey: string | null,
): ModelInfo | null {
  return (
    visibleModelGroups(models, selectedKey)
      .flatMap((group) => group.models)
      .find((model) => model.available) ?? null
  );
}

/**
 * The row that reads as a mode rather than another model.
 *
 * A picker that offers "default" as one more entry never says what picking it
 * gets you. The switch makes the choice binary — a default, or an override —
 * and the tooltip names the model the default currently lands on, which is the
 * only thing the reader actually wanted to know.
 */
function DefaultRow({
  isDefault,
  tooltip,
  disabled,
  onToggle,
}: {
  isDefault: boolean;
  tooltip: string;
  disabled: boolean;
  onToggle: (useDefault: boolean) => void;
}) {
  return (
    <div
      role="group"
      className="bg-accent/40 flex items-center gap-2 rounded-sm px-2 py-2"
    >
      <span className="text-sm font-medium">Default</span>
      <WithTooltip label={tooltip} side="top">
        <button
          type="button"
          aria-label="About Default"
          className="text-muted-foreground hover:text-foreground focus-visible:ring-ring rounded-sm focus-visible:ring-2 focus-visible:outline-none"
        >
          <Info className="size-3.5" />
        </button>
      </WithTooltip>
      <Switch
        className="ml-auto"
        checked={isDefault}
        disabled={disabled}
        onCheckedChange={onToggle}
      />
    </div>
  );
}

/**
 * Per-chat model selector for the message bar. `null` means "use the default"
 * — the global default model, or the server's own when none is set.
 *
 * `defaultKey` is the catalog key the server says that fallback resolves to,
 * so the default can be named rather than described. It is absent only when
 * the server's fallback is not something the catalog can name, and the copy
 * then says nothing rather than guessing.
 */
export function ModelMenu({
  models,
  value,
  defaultKey = null,
  disabled,
  onChange,
}: {
  models: ModelInfo[];
  value: string | null;
  defaultKey?: string | null;
  disabled?: boolean;
  onChange: (key: ModelSelectionKey | null) => void | Promise<void>;
}) {
  const known = modelForSelection(models, value);
  const canonical = canonicalModelSelection(models, value);
  const isDefault = value === null;
  const resolvedDefault = modelForSelection(models, defaultKey);
  const anyAvailable = models.some((model) => model.available);

  const label = isDefault ? "Default" : (known?.display_name ?? `${value} (unavailable)`);
  // The pill names the default's resolution too: the reader is hovering the
  // control precisely because "Default" does not tell them what will run.
  const triggerLabel =
    isDefault && resolvedDefault
      ? `Model: Default (${resolvedDefault.display_name})`
      : `Model: ${label}`;
  const defaultTooltip = resolvedDefault
    ? isDefault
      ? `New turns run against the default model. Currently: ${resolvedDefault.display_name}.`
      : "Override active. Toggle Default to go back to the default model."
    : "New turns run against the default model.";

  // The mark of whatever will actually run, so the pill reads the same whether
  // the model was chosen here or inherited.
  const pillModel = known ?? (isDefault ? resolvedDefault : null);

  // Controlled so every path that changes the chat's model closes the menu —
  // including the Default switch, which is a selection like any row but is not
  // a menu item Radix would close for us.
  const [open, setOpen] = useState(false);

  return (
    <DropdownMenu open={open} onOpenChange={setOpen}>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          className="h-8 max-w-56 gap-2"
          disabled={disabled}
          aria-label={triggerLabel}
          title={triggerLabel}
        >
          {pillModel ? (
            <ProviderIcon
              provider={pillModel.provider}
              modelId={pillModel.id}
              className="size-4"
            />
          ) : (
            <Atom className="size-4 text-muted-foreground" />
          )}
          <span className="truncate">{label}</span>
          <ChevronDown className="size-4 opacity-50" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        side="top"
        className="model-menu-content w-80 overflow-y-auto p-0"
      >
        <div className="flex flex-col gap-1 p-1">
          <DefaultRow
            isDefault={isDefault}
            tooltip={defaultTooltip}
            // With nothing available, turning the default off has nowhere to
            // land — freeze the switch instead of letting it snap back;
            // flipping back *to* the default must always stay possible.
            disabled={Boolean(disabled) || (isDefault && !anyAvailable)}
            onToggle={(useDefault) => {
              if (useDefault) {
                void onChange(null);
                setOpen(false);
                return;
              }
              // Turning the default off has to land on something; the first
              // model as the menu renders them, so the check appears at the
              // top of the list rather than mid-scroll.
              const first = firstAvailableModel(models, canonical);
              if (first) {
                void onChange(first.key);
                setOpen(false);
              }
            }}
          />

          {!anyAvailable && (
            <div>
              <DropdownMenuSeparator />
              <p className="text-muted-foreground px-2 py-2 text-sm">
                Configure a provider in Settings to choose a model.
              </p>
            </div>
          )}

          {visibleModelGroups(models, canonical).map((group) => (
            <div key={group.provider}>
              <DropdownMenuSeparator />
              {group.models.map((model) => {
                const selected = !isDefault && canonical === model.key;
                return (
                  <DropdownMenuItem
                    key={model.key}
                    disabled={disabled || !model.available}
                    // Picking a model is the whole point of opening this, so
                    // the menu closes on it. Re-selecting what is already
                    // chosen still closes: the reader asked for this model and
                    // has it, and holding the menu open would read as a
                    // dropped click.
                    onSelect={() => {
                      if (selected || !model.available) return;
                      void onChange(model.key);
                    }}
                    className={cn(
                      "flex items-center gap-2",
                      isDefault && "opacity-60",
                    )}
                  >
                    <ProviderIcon
                      provider={model.provider}
                      modelId={model.id}
                      className="size-4 shrink-0"
                    />
                    <span className="text-sm">{model.display_name}</span>
                    {selected && <Check className="ml-auto size-4" />}
                  </DropdownMenuItem>
                );
              })}
            </div>
          ))}

          {!isDefault && !known && (
            <div>
              <DropdownMenuSeparator />
              <DropdownMenuItem disabled className="flex items-center gap-2">
                <span className="text-sm">{value}</span>
                <Check className="ml-auto size-4" />
              </DropdownMenuItem>
            </div>
          )}
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/**
 * Every effort level in ascending order, with its menu label.
 *
 * "Off" rather than "None" for the lowest level, because the menu already has
 * a default: one means "do not reason", the other means "leave the provider's
 * own default alone".
 */
const REASONING_EFFORT_SCALE: { value: ReasoningEffort; label: string }[] = [
  { value: "none", label: "Off" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "xhigh", label: "X-high" },
  { value: "max", label: "Max" },
];

const REASONING_EFFORT_LABELS: Record<ReasoningEffort, string> = Object.fromEntries(
  REASONING_EFFORT_SCALE.map((option) => [option.value, option.label]),
) as Record<ReasoningEffort, string>;

/**
 * The levels to offer for a model, ordered by the scale rather than by the
 * order the server happened to send. Filtering against the known scale also
 * drops a level a newer server knows about and this build does not, so the
 * menu never offers an option it cannot label.
 */
export function reasoningEffortOptions(
  accepted: readonly ReasoningEffort[],
): { value: ReasoningEffort; label: string }[] {
  return REASONING_EFFORT_SCALE.filter((option) => accepted.includes(option.value));
}

/**
 * Per-chat reasoning-effort selector, shown next to the model picker. `null`
 * means "use the provider default".
 *
 * `levels` is the selected model's accepted range, and the menu offers exactly
 * those, since no model takes the whole scale. The caller hides the control
 * entirely when that range is empty. A level already stored on the chat still
 * labels the trigger even when the current model does not accept it — the chat
 * keeps its choice, and the server degrades it to the closest level the model
 * does take rather than sending one the model would reject.
 */
export function ReasoningEffortMenu({
  levels,
  value,
  disabled,
  onChange,
}: {
  levels: readonly ReasoningEffort[];
  value: ReasoningEffort | null;
  disabled?: boolean;
  onChange: (effort: ReasoningEffort | null) => void | Promise<void>;
}) {
  const options = reasoningEffortOptions(levels);
  const isDefault = value === null;
  const label = isDefault ? "Default" : REASONING_EFFORT_LABELS[value];
  const [open, setOpen] = useState(false);
  return (
    <DropdownMenu open={open} onOpenChange={setOpen}>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          className="h-8 gap-1.5"
          disabled={disabled}
          aria-label={`Reasoning effort: ${label}`}
          title={`Reasoning effort: ${label}`}
        >
          <Gauge className="size-4 text-muted-foreground" />
          <span className="truncate">{label}</span>
          <ChevronDown className="size-4 opacity-50" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        side="top"
        className="model-menu-content w-80 overflow-y-auto p-0"
      >
        <div className="flex flex-col gap-1 p-1">
          <DefaultRow
            isDefault={isDefault}
            tooltip={
              isDefault
                ? "The provider decides how hard the model thinks."
                : "Override active. Toggle Default to let the provider decide."
            }
            disabled={Boolean(disabled)}
            onToggle={(useDefault) => {
              if (useDefault) {
                void onChange(null);
                setOpen(false);
                return;
              }
              const first = options[0];
              if (first) {
                void onChange(first.value);
                setOpen(false);
              }
            }}
          />

          <DropdownMenuSeparator />

          {options.map((option) => {
            const selected = !isDefault && value === option.value;
            return (
              <DropdownMenuItem
                key={option.value}
                disabled={disabled}
                onSelect={() => {
                  if (selected) return;
                  void onChange(option.value);
                }}
                className={cn(
                  "flex items-center gap-2",
                  isDefault && "opacity-60",
                )}
              >
                <span className="text-sm">{option.label}</span>
                {selected && <Check className="ml-auto size-4" />}
              </DropdownMenuItem>
            );
          })}
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/**
 * Per-chat citation-format selector, shown beside the model picker. `null`
 * means "follow the global default", which is what a chat starts on.
 *
 * `defaultFormat` is the format that default currently resolves to, so the
 * Default row can name it rather than describe it — the same reason the model
 * picker names the model behind its own default.
 */
export function CitationFormatMenu({
  value,
  defaultFormat,
  disabled,
  onChange,
}: {
  value: CitationFormat | null;
  defaultFormat: CitationFormat;
  disabled?: boolean;
  onChange: (format: CitationFormat | null) => void | Promise<void>;
}) {
  const isDefault = value === null;
  const label = isDefault ? "Default" : CITATION_FORMAT_LABELS[value];
  const triggerLabel = isDefault
    ? `Citations: Default (${CITATION_FORMAT_LABELS[defaultFormat]})`
    : `Citations: ${label}`;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className="model-menu-trigger"
          disabled={disabled}
          aria-label={triggerLabel}
          title={triggerLabel}
        >
          <Quote className="size-3.5" />
          <span className="model-menu-label">{label}</span>
          <ChevronDown className="size-3.5" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        side="top"
        className="model-menu-content w-80 overflow-y-auto p-0"
      >
        <div className="flex flex-col gap-1 p-1">
          <DefaultRow
            isDefault={isDefault}
            tooltip={
              isDefault
                ? `New turns cite the way Settings says. Currently: ${CITATION_FORMAT_LABELS[defaultFormat]}.`
                : "Override active. Toggle Default to follow the setting again."
            }
            disabled={Boolean(disabled)}
            onToggle={(useDefault) => {
              void onChange(useDefault ? null : defaultFormat);
            }}
          />

          <DropdownMenuSeparator />

          {CITATION_FORMAT_OPTIONS.map((option) => {
            const selected = !isDefault && value === option.value;
            return (
              <DropdownMenuItem
                key={option.value}
                disabled={disabled}
                onSelect={(event) => {
                  event.preventDefault();
                  if (selected) return;
                  void onChange(option.value);
                }}
                className={cn(
                  "flex items-center gap-2",
                  isDefault && "opacity-60",
                )}
              >
                <span className="text-sm">{option.label}</span>
                {selected && <Check className="ml-auto size-4" />}
              </DropdownMenuItem>
            );
          })}
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
