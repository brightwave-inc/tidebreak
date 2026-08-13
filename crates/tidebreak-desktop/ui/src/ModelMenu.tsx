import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import {
  Atom,
  Check,
  ChevronDown,
  Gauge,
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
import { useManagedPolicy } from "./managedPolicy";
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
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

const CACHE_CHANGE_WARNING =
  "This change may prevent prompt cache reuse, increasing cost and latency on the next turn.";

function warnAboutPromptCacheChange() {
  toast.warning("Prompt cache may not be reused", {
    description: CACHE_CHANGE_WARNING,
  });
}

/**
 * The order the vendors are listed in, ahead of any the catalog carries that
 * this build does not know about. Fixed rather than first-seen so the list does
 * not reshuffle when a provider is configured or a custom endpoint gains a
 * model — muscle memory is worth more here than catalog order.
 */
const PROVIDER_ORDER: readonly ProviderKind[] = [
  "openai",
  "anthropic",
  "xai",
  "gemini",
  "fireworks",
  "together",
  "openrouter",
  "ollama",
];

/** Honest warning for routes Tidebreak must run without host tools. */
export function ModelToolCapabilityChip({ model }: { model: ModelInfo }) {
  if (model.supports_tools) return null;
  return (
    <WithTooltip
      label="Tidebreak runs this model as chat-only because function tools are unsupported or tool use cannot yet be continued safely"
      side="top"
    >
      <span
        className="text-muted-foreground border-border rounded-full border px-1.5 py-0.5 text-[0.65rem] leading-none"
        aria-label="Chat only. Function tools are unsupported or cannot yet be continued safely in Tidebreak."
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
 * Which provider rail the picker should open on.
 *
 * Prefers the provider of the model that will run, then the first group the
 * menu actually shows. `null` only when there is no group at all.
 */
export function pickerProviderForSelection(
  groups: readonly { provider: ProviderKind }[],
  selected: Pick<ModelInfo, "provider"> | null,
): ProviderKind | null {
  if (selected) {
    const match = groups.find((group) => group.provider === selected.provider);
    if (match) return match.provider;
  }
  return groups[0]?.provider ?? null;
}

/** One icon on the picker rail: connected providers and ones still to set up. */
export type PickerRailEntry = {
  provider: ProviderKind;
  connected: boolean;
  modelCount: number;
};

/**
 * Every provider the catalog knows, in {@link PROVIDER_ORDER}.
 *
 * Connected groups (something can run) sit with the unconfigured ones so the
 * rail is a map of the catalog rather than only what this install already
 * unlocked. The gateway stays off the unconfigured side: it has no key to
 * paste. A managed profile only lists what can already run.
 */
export function pickerRailEntries(
  models: readonly ModelInfo[],
  providers: readonly ProviderInfo[],
  selectedKey: string | null,
  managed: boolean = false,
): PickerRailEntry[] {
  const connected = visibleModelGroups(models, selectedKey).map(
    (group) => ({
      provider: group.provider,
      connected: true,
      modelCount: group.models.length,
    }),
  );
  const seen = new Set(connected.map((entry) => entry.provider));
  const unconfigured = notConnectedProviders(models, providers, managed)
    .filter((entry) => !seen.has(entry.provider))
    .map((entry) => ({
      provider: entry.provider,
      connected: false,
      modelCount: entry.modelCount,
    }));

  const byProvider = new Map<ProviderKind, PickerRailEntry>();
  for (const entry of [...connected, ...unconfigured]) {
    byProvider.set(entry.provider, entry);
  }

  const ordered: PickerRailEntry[] = [];
  for (const provider of PROVIDER_ORDER) {
    const found = byProvider.get(provider);
    if (found) {
      ordered.push(found);
      byProvider.delete(provider);
    }
  }
  for (const found of byProvider.values()) {
    ordered.push(found);
  }
  return ordered;
}

/**
 * The groups the composer picker shows: every catalog row for a provider
 * that can run something, plus the group holding the current selection even
 * when that row cannot run. A provider with nothing usable is a rail icon
 * that offers setup, not an empty model list.
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
 * Providers the catalog knows models for but that cannot serve any of them —
 * no credential, or switched off. One row each, so the picker admits the
 * catalog is larger than a fresh install shows instead of leaving the rest of
 * it undiscoverable.
 *
 * The gateway is left out: what it serves is policy, not a key the reader can
 * paste in, so a setup CTA would point at nothing they can do. A managed
 * profile drops every row for the same reason — the server refuses each
 * provider credential write, so setting one up is not something the reader can
 * do there either.
 */
export function notConnectedProviders(
  models: readonly ModelInfo[],
  providers: readonly ProviderInfo[],
  managed: boolean = false,
): { provider: ProviderKind; modelCount: number }[] {
  if (managed) return [];
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
 * The first available row *as the menu renders them*. `null` when nothing
 * can run.
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
  onSetUpProvider: (provider: ProviderKind) => void;
} {
  const navigate = useNavigate();
  const providerSettingsPath: string = "/settings/providers";
  return {
    onSetUpProvider: (provider) =>
      void navigate({
        to: providerSettingsPath,
        search: { provider, focus: "credential" },
      }),
  };
}

/**
 * Per-chat model selector for the message bar.
 *
 * `null` still means the chat follows Settings, but the picker does not
 * expose that as a mode. The trigger and the check both name the model that
 * will actually run. A fresh install with nothing credentialed says
 * "No model" rather than inventing a selection.
 *
 * `defaultKey` is the catalog key the server says that fallback resolves
 * to. It is only treated as selected when that row can actually run.
 *
 * The list is every catalog row for the selected provider.
 */
export function ModelMenu({
  models,
  value,
  defaultKey = null,
  disabled,
  providers = [],
  onSetUpProvider,
  onChange,
}: {
  models: ModelInfo[];
  value: string | null;
  defaultKey?: string | null;
  disabled?: boolean;
  /** Provider status, for the unconfigured rail icons. Empty until it loads. */
  providers?: ProviderInfo[];
  /** Open the settings card for a provider, on its credential field. */
  onSetUpProvider: (provider: ProviderKind) => void;
  onChange: (key: ModelSelectionKey | null) => void | Promise<void>;
}) {
  const known = modelForSelection(models, value);
  const { managed } = useManagedPolicy();
  const canonical = canonicalModelSelection(models, value);
  const isDefault = value === null;
  const resolvedDefault = modelForSelection(models, defaultKey);
  // A catalog hit is not enough: first install still resolves the boot
  // default, and that row cannot run until its provider is configured.
  const usableDefault =
    resolvedDefault?.available === true ? resolvedDefault : null;
  const anyAvailable = models.some((model) => model.available);
  const groups = visibleModelGroups(models, canonical);
  const rail = pickerRailEntries(models, providers, canonical, managed);

  const label = isDefault
    ? (usableDefault?.display_name ?? "No model")
    : (known?.display_name ?? `${value} (unavailable)`);
  const triggerLabel =
    isDefault && !usableDefault ? "No model selected" : `Model: ${label}`;

  // The mark of whatever will actually run, so the pill reads the same whether
  // the model was chosen here or inherited from Settings.
  const pillModel = known ?? (isDefault ? usableDefault : null);

  const [open, setOpen] = useState(false);
  const [activeProvider, setActiveProvider] = useState<ProviderKind | null>(
    () => pickerProviderForSelection(rail, known ?? usableDefault),
  );
  const activeRail =
    rail.find((entry) => entry.provider === activeProvider) ?? rail[0] ?? null;
  const activeGroup =
    groups.find((group) => group.provider === activeRail?.provider) ?? null;
  const showProviderRail = rail.length > 0;

  function openMenu(next: boolean) {
    if (next) {
      setActiveProvider(
        pickerProviderForSelection(rail, known ?? usableDefault),
      );
    }
    setOpen(next);
  }

  return (
    <DropdownMenu open={open} onOpenChange={openMenu}>
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
        collisionPadding={12}
        className="model-menu-content w-80 p-0"
      >
        {showProviderRail && (
          <div
            className="flex min-h-0 w-11 shrink-0 flex-col gap-1 overflow-y-auto border-r border-border bg-muted/30 p-1"
            role="tablist"
            aria-label="Providers"
          >
            {rail.map((entry) => {
              const selected = activeRail?.provider === entry.provider;
              const mark = groups.find((group) => group.provider === entry.provider)
                ?.models[0];
              return (
                <WithTooltip
                  key={entry.provider}
                  label={
                    entry.connected
                      ? providerLabel(entry.provider)
                      : `Connect ${providerLabel(entry.provider)}`
                  }
                  side="right"
                >
                  <button
                    type="button"
                    role="tab"
                    aria-selected={selected}
                    aria-label={
                      entry.connected
                        ? providerLabel(entry.provider)
                        : `Connect ${providerLabel(entry.provider)}`
                    }
                    className={cn(
                      "relative flex size-9 items-center justify-center rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring",
                      selected ? "bg-accent" : "hover:bg-accent/60",
                      !entry.connected && "opacity-45 hover:opacity-80",
                    )}
                    onClick={() => setActiveProvider(entry.provider)}
                  >
                    <ProviderIcon
                      provider={mark?.vendor ?? entry.provider}
                      modelId={mark?.id}
                      className="size-4"
                    />
                    {selected && (
                      <span
                        aria-hidden
                        className="absolute -right-1 top-1/2 h-4 w-0.5 -translate-y-1/2 rounded-l-full bg-primary"
                      />
                    )}
                  </button>
                </WithTooltip>
              );
            })}
          </div>
        )}

        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <div className="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto p-1">
          {!anyAvailable && (
            <p className="text-muted-foreground px-2 py-2 text-sm">
              {managed
                ? "Models are provided by your organization's gateway. None are available yet — contact your administrator."
                : "Configure a provider in Settings to choose a model."}
            </p>
          )}

          {activeRail && !activeRail.connected && (
            <div>
              <div className="flex flex-col items-start gap-3 px-2 py-3">
                <ProviderIcon
                  provider={activeRail.provider}
                  className="size-5"
                />
                <div className="space-y-1">
                  <p className="text-sm font-medium">
                    Connect {providerLabel(activeRail.provider)}
                  </p>
                  <p className="text-muted-foreground text-xs leading-relaxed">
                    {activeRail.modelCount}{" "}
                    {activeRail.modelCount === 1 ? "model" : "models"} ready
                    after you add a key.
                  </p>
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={disabled}
                  onClick={() => {
                    onSetUpProvider(activeRail.provider);
                    setOpen(false);
                  }}
                >
                  Set up
                </Button>
              </div>
            </div>
          )}

          {activeGroup && (
            <div>
              {activeGroup.models.map((model) => {
                const selected = isDefault
                  ? usableDefault?.key === model.key
                  : canonical === model.key;
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
                      if (pillModel && pillModel.provider !== model.provider) {
                        warnAboutPromptCacheChange();
                      }
                      void onChange(model.key);
                    }}
                    className="flex items-center gap-2"
                  >
                    <ProviderIcon
                      provider={model.vendor ?? model.provider}
                      modelId={model.id}
                      className="size-4 shrink-0"
                    />
                    <span className="text-sm">{model.display_name}</span>
                    <ModelToolCapabilityChip model={model} />
                    {selected && <Check className="ml-auto size-4" />}
                  </DropdownMenuItem>
                );
              })}
            </div>
          )}

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
            warnAboutPromptCacheChange();
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
                warnAboutPromptCacheChange();
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
