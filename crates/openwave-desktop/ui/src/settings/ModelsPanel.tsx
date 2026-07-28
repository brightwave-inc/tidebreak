import { useEffect, useMemo, useState } from "react";
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
 * The roles a reader can choose a model for, in the order they matter to them:
 * the conversation first, then the work the app does on its own.
 *
 * A new role is an entry here, matching the server's role list.
 */
const ROLES: { role: ModelRole; title: string; hint: string }[] = [
  {
    role: "chat",
    title: "Chat",
    hint: "New conversations start on this model, and each one can still override it.",
  },
  {
    role: "utility",
    title: "Background work",
    hint: "Work OpenWave does on its own — compacting a long conversation, for instance — runs here, so it is not billed at your conversation model. Left automatic, it picks the cheapest model your configured providers serve; with none available, that work is skipped rather than moved onto your chat model.",
  },
];

export function ModelsPanel({
  client,
  models,
  onChanged,
}: {
  client: ApiClient;
  models: ModelInfo[];
  onChanged?: () => void;
}) {
  const [roles, setRoles] = useState<ModelRoleInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState<ModelRole | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void (async () => {
      try {
        const catalog = await client.listModels();
        if (!cancelled) setRoles(catalog.roles);
      } catch (err) {
        if (!cancelled) setError(String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client]);

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
      description="Choose the provider first, then a model that provider is configured to serve. A role left automatic is resolved against whatever you have credentialed."
      busy={loading}
    >
      {loading ? (
        <p className="text-sm text-muted-foreground">Loading model settings…</p>
      ) : (
        <>
          {ROLES.map((entry) => {
            const info = roles.find((row) => row.role === entry.role);
            if (!info) return null;
            return (
              <ModelRoleRow
                key={entry.role}
                title={entry.title}
                hint={entry.hint}
                models={models}
                info={info}
                saving={saving === entry.role}
                onSelect={(selection) => void save(entry.role, selection)}
              />
            );
          })}
          {models.length === 0 && (
            <p className="text-sm text-muted-foreground">
              No models are registered yet. Configure a provider first.
            </p>
          )}
        </>
      )}
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}

/** One role's provider-then-model picker, plus what automatic resolves to. */
function ModelRoleRow({
  title,
  hint,
  models,
  info,
  saving,
  onSelect,
}: {
  title: string;
  hint: string;
  models: ModelInfo[];
  info: ModelRoleInfo;
  saving: boolean;
  onSelect: (selection: ModelSelectionKey | null) => void;
}) {
  const providers = useMemo(
    () => [...new Set(models.map((model) => model.provider))] as ProviderKind[],
    [models],
  );
  const selected = modelForSelection(models, info.selection);
  const [provider, setProvider] = useState<ProviderKind | null>(null);

  // Open on the pinned model's provider, else on one that can actually serve
  // this role, so the list beneath is never a dead end.
  useEffect(() => {
    setProvider(
      selected?.provider ??
        providers.find((kind) =>
          models.some((model) => model.provider === kind && model.available),
        ) ??
        providers[0] ??
        null,
    );
  }, [selected?.provider, providers, models]);

  const providerModels = provider
    ? models.filter((model) => model.provider === provider)
    : [];
  const canonical = canonicalModelSelection(models, info.selection);
  const selectedValue =
    selected?.provider === provider ? (canonical ?? "") : "";
  const unresolvedSelection = info.selection !== null && selected === null;

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
                {model.available ? "" : " — unavailable"}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </SettingsField>
      {unresolvedSelection && (
        <SettingsError>
          The saved model “{info.selection}” is not uniquely registered. Add it
          under the OpenAI-compatible provider, then choose it here.
        </SettingsError>
      )}
    </SettingsSection>
  );
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
