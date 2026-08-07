import { useEffect, useMemo, useRef, useState } from "react";
import type {
  ApiClient,
  ModelInfo,
  ModelRole,
  ModelRoleInfo,
  ModelSelectionKey,
  ProviderKind,
} from "../api";
import {
  canonicalModelSelection,
  modelForSelection,
  providerLabel,
} from "../ModelSelection";
import { formatTokenCount } from "../ContextUsage";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";

// Radix Select reserves the empty string for its placeholder, so the
// "automatic" choice needs a sentinel that no catalog key can collide with
// (keys are always `provider::id`).
const AUTOMATIC = "__automatic__";

/**
 * How often a managed panel re-reads the catalog. Entitlements move under the
 * gateway's feet — an admin-triggered model sync changes both the list and
 * what automatic resolves to — so the page keeps itself current instead of
 * asking the reader to refresh. Slower than the gate's session watch on
 * purpose: entitlement changes are rare, and each tick is a full catalog
 * read.
 */
const MANAGED_SYNC_WATCH_MS = 15_000;

/**
 * The roles a reader can choose a model for, in the order they matter to them:
 * the conversation first, then the work the app does on its own.
 *
 * A new role is an entry here, matching the server's role list. `managedHint`
 * is the same story told for a profile whose models all come from a gateway,
 * where "your configured providers" would name a thing the reader cannot have.
 */
const ROLES: {
  role: ModelRole;
  title: string;
  hint: string;
  /** Managed rewording, for roles whose open-experience copy names things a
   * managed reader cannot have. Roles whose story is the same either way
   * omit it. */
  managedHint?: string;
}[] = [
  {
    role: "chat",
    title: "Chat",
    hint: "New conversations start on this model, and each one can still override it.",
  },
  {
    role: "utility",
    title: "Background work",
    hint: "Work OpenWave does on its own — compacting a long conversation, for instance — runs here, so it is not billed at your conversation model. Left automatic, it picks the cheapest model your configured providers serve; with none available, that work is skipped rather than moved onto your chat model.",
    managedHint:
      "Work OpenWave does on its own — compacting a long conversation, for instance — runs here, so it is not billed at your conversation model. Left automatic, it picks the smallest model your gateway serves; with none available, that work is skipped rather than moved onto your chat model.",
  },
];

export function ModelsPanel({
  client,
  models,
  managed = false,
  onChanged,
}: {
  client: ApiClient;
  models: ModelInfo[];
  /** A managed profile's models all come from its gateway: there is no
   * provider to choose, each role offers the entitled list flat, and a stored
   * default the gateway does not serve reads as what automatic resolves to —
   * the same re-route the server applies when resolving the role. */
  managed?: boolean;
  onChanged?: () => void;
}) {
  const [roles, setRoles] = useState<ModelRoleInfo[]>([]);
  const [catalog, setCatalog] = useState<ModelInfo[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState<ModelRole | null>(null);
  const [error, setError] = useState<string | null>(null);

  // `managed` is a dependency on purpose: a policy flip mid-session re-reads
  // the catalog, so the page reshapes without a manual refresh.
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void (async () => {
      try {
        const next = await client.listModels();
        if (!cancelled) {
          setRoles(next.roles);
          setCatalog(next.models);
        }
      } catch (err) {
        if (!cancelled) setError(String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client, managed]);

  // The managed watch. A failed tick keeps the last answer — the next tick
  // retries — a tick that finds the previous read still in flight skips
  // rather than stacking requests behind a slow server, and the poll only
  // exists while the profile is managed, so the open experience keeps its
  // read-once behavior untouched.
  const watchInFlight = useRef(false);
  useEffect(() => {
    if (!managed) return;
    const timer = window.setInterval(() => {
      if (watchInFlight.current) return;
      watchInFlight.current = true;
      void client
        .listModels()
        .then((next) => {
          setRoles(next.roles);
          setCatalog(next.models);
        })
        .catch(() => undefined)
        .finally(() => {
          watchInFlight.current = false;
        });
    }, MANAGED_SYNC_WATCH_MS);
    return () => window.clearInterval(timer);
  }, [client, managed]);

  // Unmanaged renders from the shell's catalog exactly as it always has;
  // managed renders from the panel's own fetch, which the watch keeps current.
  const catalogModels = managed ? (catalog ?? models) : models;
  const entitled = useMemo(
    () => catalogModels.filter((model) => model.provider === "model_gateway"),
    [catalogModels],
  );

  async function save(role: ModelRole, selection: ModelSelectionKey | null) {
    setSaving(role);
    setError(null);
    try {
      const next = await client.putModelRole(role, selection);
      setRoles((current) =>
        current.map((entry) => (entry.role === role ? next : entry)),
      );
      // The composer names its own default from the shell's catalog, which is
      // now a step behind.
      onChanged?.();
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(null);
    }
  }

  return (
    <SettingsPanel
      title="Models"
      description={
        managed
          ? "Your organization's gateway decides which models are available here. A role left automatic resolves to one of them on its own."
          : "Choose the provider first, then a model that provider is configured to serve. A role left automatic is resolved against whatever you have credentialed."
      }
      busy={loading}
    >
      {loading ? (
        <p className="text-sm text-muted-foreground">Loading model settings…</p>
      ) : (
        <>
          {ROLES.map((entry) => {
            const info = roles.find((row) => row.role === entry.role);
            if (!info) return null;
            return managed ? (
              <ManagedModelRoleRow
                key={entry.role}
                role={entry.role}
                title={entry.title}
                hint={entry.managedHint ?? entry.hint}
                models={catalogModels}
                entitled={entitled}
                info={info}
                saving={saving === entry.role}
                onSelect={(selection) => void save(entry.role, selection)}
              />
            ) : (
              <ModelRoleRow
                key={entry.role}
                role={entry.role}
                title={entry.title}
                hint={entry.hint}
                models={models}
                info={info}
                saving={saving === entry.role}
                onSelect={(selection) => void save(entry.role, selection)}
              />
            );
          })}
          {managed ? (
            <ManagedCatalogNotice entitled={entitled} />
          ) : (
            models.length === 0 && (
              <p className="text-sm text-muted-foreground">
                No models are registered yet. Configure a provider first.
              </p>
            )
          )}
        </>
      )}
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}

/**
 * What a managed reader is told when the entitled list cannot be picked from:
 * nothing synced yet, or a gateway session that has lapsed. Both are states
 * only the gateway side can fix, so the copy names the fix rather than
 * offering rows that cannot be chosen.
 */
function ManagedCatalogNotice({ entitled }: { entitled: ModelInfo[] }) {
  if (entitled.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        The gateway has not synced any models yet. They appear here as soon as
        a sync completes.
      </p>
    );
  }
  if (!entitled.some((model) => model.available)) {
    return (
      <p className="text-sm text-muted-foreground">
        Sign in to your gateway to choose models.
      </p>
    );
  }
  return null;
}

/**
 * One role under managed policy: no provider dropdown — there is exactly one
 * provider — just the flat list of models the gateway is entitled to serve.
 *
 * A stored selection the gateway does not serve presents as the automatic
 * choice rather than a dead pin, mirroring how the server resolves it. The
 * pin itself stays stored — a profile returned to the open experience gets
 * its selection back — which the row says out loud, and the automatic entry
 * doubles as the way to clear it: with a dead pin the trigger carries no
 * value of its own (it shows the automatic label as placeholder), so choosing
 * the automatic entry is a real change that persists `null`.
 */
function ManagedModelRoleRow({
  role,
  title,
  hint,
  models,
  entitled,
  info,
  saving,
  onSelect,
}: {
  role: ModelRole;
  title: string;
  hint: string;
  models: ModelInfo[];
  entitled: ModelInfo[];
  info: ModelRoleInfo;
  saving: boolean;
  onSelect: (selection: ModelSelectionKey | null) => void;
}) {
  const selected = modelForSelection(models, info.selection);
  const selectableEntitled = entitled.filter((model) =>
    modelSupportsRole(model, role),
  );
  const incompatiblePin =
    selected !== null &&
    info.selection !== null &&
    !modelSupportsRole(selected, role);
  const gatewayServed =
    selected !== null &&
    selected.provider === "model_gateway" &&
    selected.available &&
    !incompatiblePin;
  const deadPin = !gatewayServed && info.selection !== null;
  // The kept-and-restored story belongs to a BYOK pin surviving managed mode.
  // A gateway pin that is merely unavailable — signed out, say — has no such
  // story, so it gets no notice.
  const keptByokPin =
    deadPin && selected !== null && selected.provider !== "model_gateway";
  const automatic = automaticLabel(models, info);
  const selectValue =
    gatewayServed && selected ? selected.key : deadPin ? "" : AUTOMATIC;

  return (
    <SettingsSection title={title}>
      <p className="text-sm text-muted-foreground">{hint}</p>
      {keptByokPin && (
        <p className="text-sm text-muted-foreground">
          {`Your previous ${providerLabel(selected.provider)} selection is kept and restored if this device leaves managed mode — picking a model here replaces it.`}
        </p>
      )}
      <SettingsField label="Model">
        <Select
          value={selectValue}
          disabled={
            saving ||
            selectableEntitled.length === 0 ||
            !selectableEntitled.some((model) => model.available)
          }
          onValueChange={(value) => {
            onSelect(value === AUTOMATIC ? null : (value as ModelSelectionKey));
          }}
        >
          <SelectTrigger aria-label={`${title} model`}>
            <SelectValue placeholder={automatic} />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={AUTOMATIC}>
              {deadPin
                ? `${automatic} (clears your previous ${
                    selected?.display_name ?? info.selection
                  } pin)`
                : automatic}
            </SelectItem>
            {selectableEntitled.map((model) => (
              <SelectItem
                key={model.key}
                value={model.key}
                disabled={!model.available}
              >
                {model.display_name}
                {contextWindowSuffix(model)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </SettingsField>
      {incompatiblePin && (
        <SettingsError>
          The saved model “{selected?.display_name ?? info.selection}” cannot
          enforce the strict structured responses this role requires. Pick a
          compatible model or return to automatic.
        </SettingsError>
      )}
    </SettingsSection>
  );
}

/** One role's provider-then-model picker, plus what automatic resolves to. */
function ModelRoleRow({
  role,
  title,
  hint,
  models,
  info,
  saving,
  onSelect,
}: {
  role: ModelRole;
  title: string;
  hint: string;
  models: ModelInfo[];
  info: ModelRoleInfo;
  saving: boolean;
  onSelect: (selection: ModelSelectionKey | null) => void;
}) {
  const selectableModels = useMemo(
    () => models.filter((model) => modelSupportsRole(model, role)),
    [models, role],
  );
  const providers = useMemo(
    () =>
      [
        ...new Set(selectableModels.map((model) => model.provider)),
      ] as ProviderKind[],
    [selectableModels],
  );
  const selected = modelForSelection(models, info.selection);
  const incompatiblePin =
    selected !== null &&
    info.selection !== null &&
    !modelSupportsRole(selected, role);
  const [provider, setProvider] = useState<ProviderKind | null>(null);

  // Open on the pinned model's provider, else on one that can actually serve
  // this role, so the list beneath is never a dead end.
  useEffect(() => {
    setProvider(
      (incompatiblePin ? null : selected?.provider) ??
        providers.find((kind) =>
          selectableModels.some(
            (model) => model.provider === kind && model.available,
          ),
        ) ??
        providers[0] ??
        null,
    );
  }, [incompatiblePin, selected?.provider, providers, selectableModels]);

  const providerModels = provider
    ? selectableModels.filter((model) => model.provider === provider)
    : [];
  const canonical = canonicalModelSelection(models, info.selection);
  const selectedValue =
    !incompatiblePin && selected?.provider === provider ? (canonical ?? "") : "";
  const unresolvedSelection = info.selection !== null && selected === null;
  // A pin left behind by the retired additive gateway mode. The catalog has
  // no gateway models on an unmanaged profile, so it resolves to nothing —
  // and the openai_compatible advice below cannot apply to it.
  const retiredGatewayPin =
    unresolvedSelection &&
    (info.selection?.startsWith("model_gateway::") ?? false);

  return (
    <SettingsSection title={title}>
      <p className="text-sm text-muted-foreground">{hint}</p>
      <SettingsField label="Provider">
        <Select
          value={provider ?? ""}
          disabled={saving || providers.length === 0}
          onValueChange={(value) =>
            setProvider((value || null) as ProviderKind | null)
          }
        >
          {/* Both rows carry the same visible labels, so the accessible name
              names the role as well. */}
          <SelectTrigger aria-label={`${title} provider`}>
            <SelectValue placeholder="No providers configured" />
          </SelectTrigger>
          <SelectContent>
            {providers.map((kind) => {
              const usable = models.some(
                (model) => model.provider === kind && model.available,
              );
              return (
                <SelectItem key={kind} value={kind}>
                  {providerLabel(kind)}
                  {usable ? "" : " — unavailable"}
                </SelectItem>
              );
            })}
          </SelectContent>
        </Select>
      </SettingsField>
      <SettingsField
        label="Model"
        hint="Unavailable models remain visible for clarity but cannot be selected until their provider is enabled and credentialed."
      >
        <Select
          value={selectedValue === "" ? AUTOMATIC : selectedValue}
          disabled={saving || provider === null}
          onValueChange={(value) => {
            onSelect(value === AUTOMATIC ? null : (value as ModelSelectionKey));
          }}
        >
          <SelectTrigger aria-label={`${title} model`}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value={AUTOMATIC}>
              {automaticLabel(models, info)}
            </SelectItem>
            {providerModels.map((model) => (
              <SelectItem
                key={model.key}
                value={model.key}
                disabled={!model.available}
              >
                {model.display_name}
                {contextWindowSuffix(model)}
                {model.available ? "" : " — unavailable"}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </SettingsField>
      {retiredGatewayPin ? (
        <SettingsError>
          The saved model “{info.selection}” came from a model gateway, and
          this profile is not connected to one. Pick a model here, or connect
          from your gateway&apos;s page.
        </SettingsError>
      ) : incompatiblePin ? (
        <SettingsError>
          The saved model “{selected?.display_name ?? info.selection}” cannot
          enforce the strict structured responses this role requires. Pick a
          compatible model or return to automatic.
        </SettingsError>
      ) : (
        unresolvedSelection && (
          <SettingsError>
            The saved model “{info.selection}” is not uniquely registered. Add
            it under the OpenAI-compatible provider, then choose it here.
          </SettingsError>
        )
      )}
    </SettingsSection>
  );
}

/** Whether a catalog row carries the capabilities one named role requires. */
function modelSupportsRole(model: ModelInfo, role: ModelRole): boolean {
  return role !== "utility" || model.supports_structured_output;
}

/**
 * How much conversation a model can hold, beside its name.
 *
 * The single number that most often decides between two otherwise similar
 * models, and it was only visible by picking one and watching the meter.
 */
function contextWindowSuffix(model: ModelInfo): string {
  const context = model.context_window
    ? ` — ${formatTokenCount(model.context_window)} context`
    : "";
  return `${context}${model.supports_tools ? "" : " — chat only"}`;
}

/**
 * What "automatic" currently means, named rather than implied.
 *
 * Only the server can say which model the choice lands on — it resolves a role
 * against provider readiness — including that it lands on nothing at all.
 */
function automaticLabel(models: ModelInfo[], info: ModelRoleInfo): string {
  const resolved = modelForSelection(models, info.resolved_key);
  if (resolved) return `Automatic — ${resolved.display_name}`;
  if (info.resolved_key) return `Automatic — ${info.resolved_key}`;
  return "Automatic — none available";
}
