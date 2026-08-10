import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import {
  Atom,
  Check,
  ChevronDown,
  ChevronRight,
  Gauge,
  Info,
} from "lucide-react";
import type {
  ModelInfo,
  ModelSelectionKey,
  ProviderInfo,
  ProviderKind,
  ReasoningEffort,
} from "./api";
import {
  canonicalModelSelection,
  modelForSelection,
  providerLabel,
} from "./ModelSelection";
import { isModelVisible, type ModelVisibilityOverrides } from "./modelVisibility";
import { useUiStore } from "./UiStore";
import { Button } from "@/components/ui/button";
import { ProviderIcon } from "./ProviderIcons";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
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
const PROVIDER_ORDER: readonly ProviderKind[] = [
  "anthropic",
  "openai",
  "xai",
  "gemini",
  "vertex",
  "bedrock",
  "fireworks",
  "together",
];

const UNVERIFIED_TOOLTIP =
  "OpenWave hasn't verified tool-calling and streaming for this model; issues are likely the model or provider, not the app";

export function ModelVerificationChip({ model }: { model: ModelInfo }) {
  if (model.verification !== "unverified") return null;
  return (
    <WithTooltip label={UNVERIFIED_TOOLTIP} side="top">
      <span
        className="text-muted-foreground border-border rounded-full border px-1.5 py-0.5 text-[0.65rem] leading-none"
        aria-label={`Unverified. ${UNVERIFIED_TOOLTIP}`}
      >
        Unverified
      </span>
    </WithTooltip>
  );
}

/** Honest warning for routes OpenWave must run without host tools. */
export function ModelToolCapabilityChip({ model }: { model: ModelInfo }) {
  if (model.supports_tools) return null;
  return (
    <WithTooltip
      label="OpenWave runs this model as chat-only because function tools are unsupported or tool use cannot yet be continued safely"
      side="top"
    >
      <span
        className="text-muted-foreground border-border rounded-full border px-1.5 py-0.5 text-[0.65rem] leading-none"
        aria-label="Chat only. Function tools are unsupported or cannot yet be continued safely in OpenWave."
      >
        Chat only
      </span>
    </WithTooltip>
  );
}

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
 * model that can run, holding the models that are effectively visible — plus,
 * whatever its visibility or availability, the row holding the current
 * selection, so an active override never vanishes from the menu that set it.
 * A provider with nothing usable renders as no group at all rather than a run
 * of disabled rows; discovery of the full registry belongs to the settings
 * Models surface, and the footer points at it.
 *
 * Visibility filtering is presentation only. It never changes what a chat is
 * set to, and a chat pinned to a hidden model keeps running against it.
 */
export function visibleModelGroups(
  models: readonly ModelInfo[],
  selectedKey: string | null,
  overrides: ModelVisibilityOverrides = {},
): { provider: ProviderKind; models: ModelInfo[] }[] {
  return groupByProvider(models)
    .map((group) => ({
      provider: group.provider,
      models: group.models.filter(
        (model) =>
          isModelVisible(model, overrides) || model.key === selectedKey,
      ),
    }))
    .filter(
      (group) =>
        group.models.some((model) => model.available) ||
        (selectedKey !== null &&
          group.models.some((model) => model.key === selectedKey)),
    );
}

/**
 * Providers the catalog knows models for but that cannot serve any of them —
 * no credential, or switched off. One row each, so the picker admits the
 * catalog is larger than a fresh install shows instead of leaving the rest of
 * it undiscoverable.
 *
 * The gateway is left out: what it serves is policy, not a key the reader can
 * paste in, so a setup CTA would point at nothing they can do.
 */
export function notConnectedProviders(
  models: readonly ModelInfo[],
  providers: readonly ProviderInfo[],
): { provider: ProviderKind; modelCount: number }[] {
  const status = new Map(providers.map((info) => [info.kind, info]));
  return groupByProvider(models)
    .filter((group) => {
      if (group.provider === "model_gateway") return false;
      // Anything that can already run is connected however it got there.
      if (group.models.some((model) => model.available)) return false;
      const info = status.get(group.provider);
      return !info || !info.enabled || !info.has_credential;
    })
    .map((group) => ({
      provider: group.provider,
      modelCount: group.models.length,
    }));
}

/**
 * Models that could run right now but are not listed, which is what the footer
 * offers to undo. The current selection is excluded because the picker renders
 * it regardless — counting a row the reader can see as hidden reads as a bug.
 */
export function hiddenModelCount(
  models: readonly ModelInfo[],
  selectedKey: string | null,
  overrides: ModelVisibilityOverrides = {},
): number {
  return models.filter(
    (model) =>
      model.available &&
      model.key !== selectedKey &&
      !isModelVisible(model, overrides),
  ).length;
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
  overrides: ModelVisibilityOverrides = {},
): ModelInfo | null {
  return (
    visibleModelGroups(models, selectedKey, overrides)
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
export function DefaultRow({
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
 * The picker's two ways out into settings, as one hook so both composers deep
 * link identically.
 *
 * The path is typed through a `string` because the settings sections are
 * registered from a runtime table: TanStack's generated route union contains
 * `/settings` but not each literal child. The search params are `providersSearch`
 * in `settings/sections.tsx` — which card to open, and whether to put the
 * cursor in its credential field.
 */
export function useModelSettingsNav(): {
  onManageModels: () => void;
  onSetUpProvider: (provider: ProviderKind) => void;
} {
  const navigate = useNavigate();
  const providerSettingsPath: string = "/settings/providers";
  return {
    onManageModels: () => void navigate({ to: providerSettingsPath }),
    onSetUpProvider: (provider) =>
      void navigate({
        to: providerSettingsPath,
        search: { provider, focus: "credential" },
      }),
  };
}

/**
 * Per-chat model selector for the message bar. `null` means "use the default"
 * — the global default model, or the server's own when none is set.
 *
 * `defaultKey` is the catalog key the server says that fallback resolves to,
 * so the default can be named rather than described. It is absent only when
 * the server's fallback is not something the catalog can name, and the copy
 * then says nothing rather than guessing.
 *
 * The list is the curated one: recommended models, plus whatever the reader
 * has overridden, plus this chat's own model however it is flagged. The
 * footer and the "Not connected" rows are what make that acceptable — nothing
 * the picker leaves out is more than one click away.
 */
export function ModelMenu({
  models,
  value,
  defaultKey = null,
  disabled,
  visibilityOverrides = {},
  providers = [],
  onManageModels,
  onSetUpProvider,
  onChange,
}: {
  models: ModelInfo[];
  value: string | null;
  defaultKey?: string | null;
  disabled?: boolean;
  /**
   * The reader's per-model deviations from the curated set. Empty until
   * settings load, which shows the recommended set — the same thing a reader
   * who has never touched the setting sees, so the list does not flicker.
   */
  visibilityOverrides?: ModelVisibilityOverrides;
  /** Provider status, for the "Not connected" rows. Empty until it loads. */
  providers?: ProviderInfo[];
  /** Open the settings surface that lists every model. */
  onManageModels: () => void;
  /** Open the settings card for a provider, on its credential field. */
  onSetUpProvider: (provider: ProviderKind) => void;
  onChange: (key: ModelSelectionKey | null) => void | Promise<void>;
}) {
  const known = modelForSelection(models, value);
  const canonical = canonicalModelSelection(models, value);
  const isDefault = value === null;
  const resolvedDefault = modelForSelection(models, defaultKey);
  const anyAvailable = models.some((model) => model.available);
  const groups = visibleModelGroups(models, canonical, visibilityOverrides);
  const unconfigured = notConnectedProviders(models, providers);
  const hiddenCount = hiddenModelCount(models, canonical, visibilityOverrides);
  const notConnectedCollapsed = useUiStore(
    (state) => state.modelMenuNotConnectedCollapsed,
  );
  const toggleNotConnected = useUiStore(
    (state) => state.toggleModelMenuNotConnected,
  );

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
              provider={pillModel.vendor ?? pillModel.provider}
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
              const first = firstAvailableModel(
                models,
                canonical,
                visibilityOverrides,
              );
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

          {groups.map((group) => (
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
                      provider={model.vendor ?? model.provider}
                      modelId={model.id}
                      className="size-4 shrink-0"
                    />
                    <span className="text-sm">{model.display_name}</span>
                    <ModelVerificationChip model={model} />
                    <ModelToolCapabilityChip model={model} />
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

          {unconfigured.length > 0 && (
            <div>
              <DropdownMenuSeparator />
              {/* Deliberately a button rather than a menu item: the header and
                  the setup rows are chrome around the options, and arrowing
                  through the picker should land on models only. */}
              <button
                type="button"
                aria-expanded={!notConnectedCollapsed}
                onClick={() => toggleNotConnected()}
                className="text-muted-foreground hover:text-foreground focus-visible:ring-ring flex w-full items-center gap-1 rounded-sm px-2 py-1.5 text-xs font-medium tracking-wide uppercase focus-visible:ring-2 focus-visible:outline-none"
              >
                <ChevronRight
                  className={cn(
                    "size-3 transition-transform motion-reduce:transition-none",
                    !notConnectedCollapsed && "rotate-90",
                  )}
                />
                Not connected
              </button>
              {!notConnectedCollapsed &&
                unconfigured.map((entry) => (
                  <div
                    key={entry.provider}
                    className="flex items-center gap-2 px-2 py-1.5"
                  >
                    <ProviderIcon
                      provider={entry.provider}
                      className="size-4 shrink-0 opacity-50"
                    />
                    <span className="text-muted-foreground truncate text-sm">
                      {providerLabel(entry.provider)}
                    </span>
                    <span className="text-muted-foreground/70 text-xs whitespace-nowrap">
                      {entry.modelCount}{" "}
                      {entry.modelCount === 1 ? "model" : "models"}
                    </span>
                    <Button
                      variant="outline"
                      className="ml-auto h-6 rounded-full px-2.5 text-xs"
                      disabled={disabled}
                      onClick={() => {
                        onSetUpProvider(entry.provider);
                        setOpen(false);
                      }}
                    >
                      Set up
                    </Button>
                  </div>
                ))}
            </div>
          )}
        </div>

        {hiddenCount > 0 && (
          <div className="border-border flex items-center gap-2 border-t px-3 py-2">
            <span className="text-muted-foreground text-xs">
              {hiddenCount} {hiddenCount === 1 ? "model" : "models"} hidden
            </span>
            <button
              type="button"
              onClick={() => {
                onManageModels();
                setOpen(false);
              }}
              className="text-muted-foreground hover:text-foreground focus-visible:ring-ring ml-auto flex items-center gap-1 rounded-sm text-xs font-medium focus-visible:ring-2 focus-visible:outline-none"
            >
              Manage models
              <ChevronRight className="size-3" />
            </button>
          </div>
        )}
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
 * Per-chat reasoning effort, as a submenu of the composer's tools menu. `null`
 * means "use the provider default".
 *
 * `levels` is the selected model's accepted range, and the menu offers exactly
 * those, since no model takes the whole scale. The caller omits the submenu
 * entirely when that range is empty. A level already stored on the chat still
 * labels the row even when the current model does not accept it — the chat
 * keeps its choice, and the server degrades it to the closest level the model
 * does take rather than sending one the model would reject.
 */
export function ReasoningEffortSubMenu({
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
  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger disabled={disabled}>
        <Gauge className="size-4 text-muted-foreground" />
        <span>Reasoning</span>
        <span className="text-muted-foreground flex-1 text-right text-xs">
          {label}
        </span>
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent className="w-48">
        <DropdownMenuItem
          disabled={disabled}
          onSelect={() => {
            if (isDefault) return;
            void onChange(null);
          }}
          className="flex items-center gap-2"
        >
          <span className="text-sm">Default</span>
          {isDefault && <Check className="ml-auto size-4" />}
        </DropdownMenuItem>
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
              className="flex items-center gap-2"
            >
              <span className="text-sm">{option.label}</span>
              {selected && <Check className="ml-auto size-4" />}
            </DropdownMenuItem>
          );
        })}
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  );
}
