import { useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import { Atom, Check, ChevronDown, Gauge, Search } from "lucide-react";
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
import { familyForModelId, MODEL_ID_FAMILIES } from "./modelFamilies";
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
import { useGuidedMenu } from "./FirstTaskWalkthrough";
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
      label="Tidebreak runs this model as conversation-only because function tools are unsupported or tool use cannot yet be continued safely"
      side="top"
    >
      <span
        className="text-muted-foreground border-border rounded-full border px-1.5 py-0.5 text-2xs leading-none"
        aria-label="Conversation only. Function tools are unsupported or cannot yet be continued safely in Tidebreak."
      >
        Conversation only
      </span>
    </WithTooltip>
  );
}

/** How a rail tab or group header is drawn: a mark and a name. */
type GroupBadge = {
  label: string;
  iconProvider: ProviderKind;
  iconModelId?: string;
};

/**
 * One section of the picker: a serving provider, or one vendor's slice of a
 * gateway catalog. `id` is the rail identity; `provider` stays the route that
 * serves the rows, which routing and policy still key on.
 */
export type CatalogGroup = GroupBadge & {
  id: string;
  provider: ProviderKind;
  models: ModelInfo[];
};

/**
 * Which group a row belongs to.
 *
 * Direct providers group as themselves. A gateway catalog is mixed, so its
 * rows split by the vendor the row is branded with — the curated `vendor`
 * when the server matched one, else the open-model family its id names —
 * and only rows with neither stay under the generic gateway tab.
 */
function groupBadgeForModel(model: ModelInfo): GroupBadge & { id: string } {
  if (model.provider !== "model_gateway") {
    return {
      id: model.provider,
      label: providerLabel(model.provider),
      iconProvider: model.provider,
    };
  }
  const family = familyForModelId(model.id);
  if (family) {
    return {
      id: `model_gateway/${family.match}`,
      label: family.label,
      iconProvider: "model_gateway",
      iconModelId: family.match,
    };
  }
  if (model.vendor) {
    return {
      id: `model_gateway/${model.vendor}`,
      label: providerLabel(model.vendor),
      iconProvider: model.vendor,
    };
  }
  return {
    id: "model_gateway",
    label: providerLabel("model_gateway"),
    iconProvider: "model_gateway",
  };
}

/** Rank vendors inside a gateway catalog: known vendors first, then families. */
const GATEWAY_GROUP_ORDER: readonly string[] = [
  ...PROVIDER_ORDER.map((provider) => `model_gateway/${provider}`),
  ...MODEL_ID_FAMILIES.map((family) => `model_gateway/${family.match}`),
  "model_gateway",
];

/**
 * Catalog rows grouped for the rail: providers in {@link PROVIDER_ORDER},
 * then the rest as found — except a gateway catalog, whose groups sort by
 * {@link GATEWAY_GROUP_ORDER} so the split does not reshuffle with the
 * catalog.
 */
function groupCatalog(models: readonly ModelInfo[]): CatalogGroup[] {
  const byId = new Map<string, CatalogGroup>();
  for (const model of models) {
    const badge = groupBadgeForModel(model);
    const existing = byId.get(badge.id);
    if (existing) existing.models.push(model);
    else
      byId.set(badge.id, {
        ...badge,
        provider: model.provider,
        models: [model],
      });
  }

  const groups: CatalogGroup[] = [];
  for (const provider of PROVIDER_ORDER) {
    const found = byId.get(provider);
    if (found) {
      groups.push(found);
      byId.delete(provider);
    }
  }
  const rest = [...byId.values()];
  const gatewayRank = (group: CatalogGroup) => {
    const rank = GATEWAY_GROUP_ORDER.indexOf(group.id);
    return rank === -1 ? GATEWAY_GROUP_ORDER.length : rank;
  };
  groups.push(...rest.filter((group) => group.provider !== "model_gateway"));
  groups.push(
    ...rest
      .filter((group) => group.provider === "model_gateway")
      .sort((a, b) => gatewayRank(a) - gatewayRank(b)),
  );
  return groups;
}

/**
 * Which rail tab the picker should open on.
 *
 * Prefers the group of the model that will run, then the first group the
 * menu actually shows. `null` only when there is no group at all.
 */
export function pickerGroupForSelection(
  groups: readonly { id: string }[],
  selected: ModelInfo | null,
): string | null {
  if (selected) {
    const id = groupBadgeForModel(selected).id;
    const match = groups.find((group) => group.id === id);
    if (match) return match.id;
  }
  return groups[0]?.id ?? null;
}

/** One icon on the picker rail: connected groups and providers still to set up. */
export type PickerRailEntry = GroupBadge & {
  id: string;
  provider: ProviderKind;
  connected: boolean;
  modelCount: number;
};

/**
 * Every group the catalog knows, in {@link PROVIDER_ORDER}.
 *
 * Connected groups (something can run) sit with the unconfigured ones so the
 * rail is a map of the catalog rather than only what this install already
 * unlocked. The gateway stays off the unconfigured side: it has no key to
 * paste. A managed profile only lists what can already run. A provider whose
 * vendor a gateway group already serves is not offered for setup — its models
 * can run through the gateway.
 */
export function pickerRailEntries(
  models: readonly ModelInfo[],
  providers: readonly ProviderInfo[],
  selectedKey: string | null,
  managed: boolean = false,
): PickerRailEntry[] {
  const connected: PickerRailEntry[] = visibleModelGroups(
    models,
    selectedKey,
  ).map((group) => ({
    id: group.id,
    provider: group.provider,
    label: group.label,
    iconProvider: group.iconProvider,
    iconModelId: group.iconModelId,
    connected: true,
    modelCount: group.models.length,
  }));
  const seen = new Set(
    connected.flatMap((entry) => [
      entry.id,
      // A gateway vendor group covers the direct provider it mirrors.
      ...(entry.id.startsWith("model_gateway/")
        ? [entry.id.slice("model_gateway/".length)]
        : []),
    ]),
  );
  const unconfigured: PickerRailEntry[] = notConnectedProviders(
    models,
    providers,
    managed,
  )
    .filter((entry) => !seen.has(entry.provider))
    .map((entry) => ({
      id: entry.provider,
      provider: entry.provider,
      label: providerLabel(entry.provider),
      iconProvider: entry.provider,
      connected: false,
      modelCount: entry.modelCount,
    }));

  const byId = new Map<string, PickerRailEntry>();
  for (const entry of [...connected, ...unconfigured]) {
    byId.set(entry.id, entry);
  }

  const ordered: PickerRailEntry[] = [];
  for (const provider of PROVIDER_ORDER) {
    const found = byId.get(provider);
    if (found) {
      ordered.push(found);
      byId.delete(provider);
    }
  }
  for (const found of byId.values()) {
    ordered.push(found);
  }
  return ordered;
}

/**
 * The groups the composer picker shows: every catalog row for a group
 * that can run something, plus the group holding the current selection even
 * when that row cannot run. A provider with nothing usable is a rail icon
 * that offers setup, not an empty model list.
 */
export function visibleModelGroups(
  models: readonly ModelInfo[],
  selectedKey: string | null,
): CatalogGroup[] {
  return groupCatalog(models).flatMap((group) => {
    if (group.models.some((model) => model.available)) return [group];
    if (selectedKey === null) return [];

    // Keep an unavailable explicit selection visible so the picker explains
    // what the chat still names, but do not resurrect that provider's entire
    // disabled catalog. This is especially noisy after a gateway becomes the
    // usable route for the same upstream models.
    const selected = group.models.find((model) => model.key === selectedKey);
    return selected ? [{ ...group, models: [selected] }] : [];
  });
}

/** Models visible in cross-provider search, in the same order as browsing. */
export function matchingModels(
  groups: readonly { provider: ProviderKind; models: readonly ModelInfo[] }[],
  query: string,
): ModelInfo[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [];
  return groups.flatMap((group) =>
    group.models.filter((model) => {
      const vendor = model.vendor ? providerLabel(model.vendor) : "";
      return [
        model.display_name,
        model.id,
        providerLabel(model.provider),
        vendor,
      ].some((value) => value.toLocaleLowerCase().includes(needle));
    }),
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
  return groupCatalog(models)
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

  const guided = useGuidedMenu("model");
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const searchInput = useRef<HTMLInputElement>(null);
  const [activeGroupId, setActiveGroupId] = useState<string | null>(() =>
    pickerGroupForSelection(rail, known ?? usableDefault),
  );
  const activeRail =
    rail.find((entry) => entry.id === activeGroupId) ?? rail[0] ?? null;
  const activeGroup =
    groups.find((group) => group.id === activeRail?.id) ?? null;
  const showProviderRail = rail.length > 0;
  const searchResults = matchingModels(groups, query);
  const searching = query.length > 0;

  function openMenu(next: boolean) {
    if (guided.guided && !next) return;
    if (next) {
      setQuery("");
      setActiveGroupId(pickerGroupForSelection(rail, known ?? usableDefault));
    }
    setOpen(next);
  }

  function chooseModel(model: ModelInfo, selected: boolean) {
    if (selected || !model.available) return;
    if (pillModel && pillModel.provider !== model.provider) {
      warnAboutPromptCacheChange();
    }
    void onChange(model.key);
  }

  const visibleModels = searching ? searchResults : (activeGroup?.models ?? []);
  const spotlightKey =
    (known ?? usableDefault)?.key ??
    visibleModels.find((model) => model.available)?.key ??
    null;

  function modelRow(model: ModelInfo, showProvider: boolean = false) {
    const selected = isDefault
      ? usableDefault?.key === model.key
      : canonical === model.key;
    return (
      <DropdownMenuItem
        key={model.key}
        disabled={disabled || !model.available}
        // Picking a model is the whole point of opening this, so the menu
        // closes on it. Re-selecting the current row still closes naturally.
        onSelect={() => chooseModel(model, selected)}
        className="flex items-center gap-2"
        data-first-task-target={
          model.key === spotlightKey ? "model-choice" : undefined
        }
      >
        <ProviderIcon
          provider={model.vendor ?? model.provider}
          modelId={model.id}
          className="size-4 shrink-0"
        />
        <span className="min-w-0 flex-1 truncate text-sm">
          {model.display_name}
        </span>
        {showProvider && (
          <span className="text-muted-foreground shrink-0 text-2xs">
            {providerLabel(model.provider)}
          </span>
        )}
        <ModelToolCapabilityChip model={model} />
        {selected && <Check className="ml-auto size-4" />}
      </DropdownMenuItem>
    );
  }

  return (
    <DropdownMenu
      open={guided.open || open}
      modal={guided.modal}
      onOpenChange={openMenu}
    >
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
        data-first-task-target="model-menu"
        onEscapeKeyDown={guided.onEscapeKeyDown}
        onKeyDownCapture={(event) => {
          if (event.target === searchInput.current) return;
          if (event.metaKey || event.ctrlKey || event.altKey) return;
          if (event.key.length !== 1) return;
          event.preventDefault();
          setQuery((current) => current + event.key);
          requestAnimationFrame(() => searchInput.current?.focus());
        }}
      >
        {showProviderRail && (
          <div
            className="flex min-h-0 w-11 shrink-0 flex-col gap-1 overflow-y-auto border-r border-border bg-muted/30 p-1"
            role="tablist"
            aria-label="Providers"
          >
            {rail.map((entry) => {
              const selected = activeRail?.id === entry.id;
              const railLabel = entry.connected
                ? entry.label
                : `Connect ${entry.label}`;
              return (
                <WithTooltip key={entry.id} label={railLabel} side="right">
                  <button
                    type="button"
                    role="tab"
                    aria-selected={selected}
                    aria-label={railLabel}
                    className={cn(
                      "relative flex size-9 items-center justify-center rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring",
                      selected ? "bg-accent" : "hover:bg-accent/60",
                      !entry.connected && "opacity-45 hover:opacity-80",
                    )}
                    onClick={() => setActiveGroupId(entry.id)}
                  >
                    <ProviderIcon
                      // A rail tab wears the mark of its group: a vendor's
                      // slice of a gateway catalog gets the vendor mark, and
                      // only gateway rows with no recognizable vendor keep
                      // the generic gateway mark.
                      provider={entry.iconProvider}
                      modelId={entry.iconModelId}
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
          <div className="border-border border-b p-1.5">
            <label className="bg-muted/40 focus-within:ring-ring flex h-8 items-center gap-2 rounded-md px-2 focus-within:ring-2">
              <Search className="text-muted-foreground size-3.5 shrink-0" />
              <input
                ref={searchInput}
                type="search"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                onKeyDown={(event) => event.stopPropagation()}
                placeholder="Search models"
                aria-label="Search models"
                className="placeholder:text-muted-foreground min-w-0 flex-1 bg-transparent text-sm outline-none"
              />
            </label>
          </div>
          <div className="flex min-h-0 flex-1 flex-col gap-1 overflow-y-auto p-1">
            {!anyAvailable && (
              <p className="text-muted-foreground px-2 py-2 text-sm">
                {managed
                  ? "Models are provided by your organization's gateway. None are available yet — contact your administrator."
                  : "Configure a provider in Settings to choose a model."}
              </p>
            )}

            {!searching && activeRail && !activeRail.connected && (
              <div>
                <div className="flex flex-col items-start gap-3 px-2 py-3">
                  <ProviderIcon
                    provider={activeRail.iconProvider}
                    modelId={activeRail.iconModelId}
                    className="size-5"
                  />
                  <div className="space-y-1">
                    <p className="text-sm font-medium">
                      Connect {activeRail.label}
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

            {!searching && activeGroup && (
              <div>{activeGroup.models.map((model) => modelRow(model))}</div>
            )}

            {searching && (
              <div>
                {searchResults.length > 0 ? (
                  searchResults.map((model) => modelRow(model, true))
                ) : (
                  <p className="text-muted-foreground px-2 py-3 text-sm">
                    No models match “{query}”.
                  </p>
                )}
              </div>
            )}

            {!searching && !isDefault && !known && (
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
 *
 * "Ultra" sits above "Max". No chat model accepts it — it exists for the code
 * engines, which spell their own top rung differently (Codex `ultra`, Claude
 * Code ultracode) and are given one name here.
 */
const REASONING_EFFORT_SCALE: { value: ReasoningEffort; label: string }[] = [
  { value: "none", label: "Off" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "xhigh", label: "X-high" },
  { value: "max", label: "Max" },
  { value: "ultra", label: "Ultra" },
];

const REASONING_EFFORT_LABELS: Record<ReasoningEffort, string> =
  Object.fromEntries(
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
  return REASONING_EFFORT_SCALE.filter((option) =>
    accepted.includes(option.value),
  );
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
