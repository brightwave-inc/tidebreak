import { useId, useState } from "react";
import { toast } from "sonner";

import type {
  ApiClient,
  CustomModelConfig,
  ModelInfo,
  ProviderInfo,
} from "../api";
import { Button } from "@/components/ui/button";
import { friendlyErrorMessage } from "@/lib/utils";
import { useConfirm } from "../components/ConfirmDialog";
import { providerLabel } from "../ModelSelection";
import { CustomModelDialog } from "./CustomModelDialog";
import {
  builtInModelIds,
  configuredModelFacts,
  draftFromConfig,
  emptyDraft,
  type ModelDraft,
  rowForSave,
} from "./customModels";
import { DiscoverModelsDialog } from "./DiscoverModelsDialog";
import { SettingsError } from "./primitives";

type FormState =
  | { mode: "add"; initial: ModelDraft }
  | { mode: "edit"; index: number; initial: ModelDraft };

/**
 * A provider's models: the built-in ones by name, and the custom ones the
 * reader added, each with Edit and Remove. Add model takes a model by its API
 * id; Find models lists what the provider serves for the saved key.
 *
 * Every change saves the provider's whole custom list at once, apart from the
 * credential and endpoint the card saves separately.
 */
export function ProviderModelsSection({
  info,
  catalogModels,
  client,
  onChanged,
}: {
  info: ProviderInfo;
  /** This provider's catalog rows, built-in and custom alike. */
  catalogModels: ModelInfo[];
  client: Pick<ApiClient, "putProvider" | "discoverProviderModels">;
  onChanged: () => void;
}) {
  const headingId = useId();
  const name = providerLabel(info.kind);
  // A server that predates the list sends none; the forms then offer no
  // reasoning levels, and saves send stored levels back unchanged.
  const accepted = info.custom_reasoning_efforts ?? [];
  const builtIn = builtInModelIds(info, catalogModels);
  const builtInNames = catalogModels
    .filter((model) => builtIn.has(model.id))
    .map((model) => model.display_name);
  const replaced = (info.replaced_by_built_in ?? []).map(
    (id) => catalogModels.find((model) => model.id === id)?.display_name ?? id,
  );
  const blocker = discoveryBlocker(info);
  const [form, setForm] = useState<FormState | null>(null);
  const [discovering, setDiscovering] = useState(false);
  const [removing, setRemoving] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { confirm, dialog } = useConfirm();

  async function save(models: CustomModelConfig[]) {
    await client.putProvider(info.kind, {
      models: models.map((model) =>
        rowForSave(model, info.custom_reasoning_efforts),
      ),
    });
    onChanged();
  }

  async function saveForm(model: CustomModelConfig) {
    if (!form) return;
    const next =
      form.mode === "add"
        ? [...info.models, model]
        : info.models.map((row, index) => (index === form.index ? model : row));
    await save(next);
    toast.success(
      form.mode === "add"
        ? `Added ${modelName(model)} to ${name}`
        : `Saved ${modelName(model)}`,
    );
  }

  async function addDiscovered(models: CustomModelConfig[]) {
    await save([...info.models, ...models]);
    toast.success(
      models.length === 1
        ? `Added ${modelName(models[0])} to ${name}`
        : `Added ${models.length} models to ${name}`,
    );
  }

  async function remove(index: number) {
    const model = info.models[index];
    const confirmed = await confirm({
      title: `Remove ${modelName(model)}?`,
      description:
        "Model pickers stop offering it. Conversations that use it need another model before their next turn.",
      confirmLabel: "Remove model",
      destructive: true,
    });
    if (!confirmed) return;
    setRemoving(model.id);
    setError(null);
    try {
      await save(info.models.filter((_, itemIndex) => itemIndex !== index));
      toast.success(`Removed ${modelName(model)}`);
    } catch (err) {
      setError(friendlyErrorMessage(err, "The model could not be removed."));
    } finally {
      setRemoving(null);
    }
  }

  // One line under the heading: what to do about an empty list, and why
  // Find models cannot run yet.
  const status = [info.models.length === 0 ? emptyHint(info) : null, blocker]
    .filter(Boolean)
    .join(" ");

  const takenIds = new Set(
    info.models
      .filter((_, index) => form?.mode !== "edit" || index !== form.index)
      .map((model) => model.id),
  );

  return (
    <section
      aria-labelledby={headingId}
      className="flex flex-col gap-3 border-t border-border-subtle pt-4"
    >
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 id={headingId} className="text-sm font-semibold">
          Models
        </h3>
        <div className="flex flex-wrap gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={blocker !== null}
            onClick={() => setDiscovering(true)}
          >
            Find models
          </Button>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setForm({ mode: "add", initial: emptyDraft() })}
          >
            Add model
          </Button>
        </div>
      </div>
      {(builtInNames.length > 0 || status || replaced.length > 0) && (
        <div className="flex flex-col gap-1 text-xs text-muted-foreground">
          {builtInNames.length > 0 && (
            <p>Built in: {builtInNames.join(", ")}</p>
          )}
          {replaced.length > 0 && <p>{replacedNote(replaced)}</p>}
          {status && <p>{status}</p>}
        </div>
      )}
      {info.models.length > 0 && (
        <ul
          aria-label={`${name} custom models`}
          className="rounded-md border border-border"
        >
          {info.models.map((model, index) => (
            <CustomModelRow
              key={model.id}
              model={model}
              busy={removing === model.id}
              onEdit={() =>
                setForm({
                  mode: "edit",
                  index,
                  initial: draftFromConfig(model),
                })
              }
              onRemove={() => void remove(index)}
            />
          ))}
        </ul>
      )}
      {error && <SettingsError>{error}</SettingsError>}

      <CustomModelDialog
        open={form !== null}
        onOpenChange={(open) => {
          if (!open) setForm(null);
        }}
        mode={form?.mode ?? "add"}
        providerName={name}
        requestNote={modelRequestNote(info, name)}
        initial={form?.initial ?? EMPTY_DRAFT}
        acceptedEfforts={accepted}
        takenIds={takenIds}
        builtInIds={builtIn}
        onSave={saveForm}
      />
      <DiscoverModelsDialog
        open={discovering}
        onOpenChange={setDiscovering}
        kind={info.kind}
        providerName={name}
        usesSavedKey={info.has_credential}
        client={client}
        acceptedEfforts={accepted}
        existingCount={info.models.length}
        onAdd={addDiscovered}
      />
      {dialog}
    </section>
  );
}

const EMPTY_DRAFT = emptyDraft();

function modelName(model: CustomModelConfig): string {
  return model.display_name?.trim() || model.id;
}

/**
 * What happened to custom models a release has since built in: Tidebreak
 * uses the built-in model, and the next save drops the reader's copy.
 */
export function replacedNote(names: readonly string[]): string {
  return names.length === 1
    ? `${names[0]} is built in now, so Tidebreak uses the built-in model instead of your custom one. The next save removes your copy.`
    : `These are built in now, so Tidebreak uses the built-in models instead of your custom ones: ${names.join(", ")}. The next save removes your copies.`;
}

/**
 * How a custom model on this provider reaches it, for the Add model form:
 * with the saved key, with no key on a local endpoint, or not until a key is
 * saved.
 */
export function modelRequestNote(info: ProviderInfo, name: string): string {
  if (info.kind === "openai" && info.auth_mode === "chatgpt") {
    return "Custom OpenAI models need an API key. ChatGPT sign-in runs only the built-in models.";
  }
  if (info.has_credential) {
    return `Tidebreak sends this model's requests to ${name} with the key you saved.`;
  }
  if (info.kind === "ollama" || info.kind === "openai_compatible") {
    return `Tidebreak sends this model's requests to ${name}. This endpoint needs no key.`;
  }
  return `Tidebreak sends this model's requests to ${name} once you save an API key.`;
}

/**
 * Why Find models cannot run for this provider yet, or `null` when it can.
 * Discovery spends the saved key, so a provider without one has nothing to
 * ask with.
 */
export function discoveryBlocker(info: ProviderInfo): string | null {
  if (info.kind === "openai" && info.auth_mode === "chatgpt") {
    return "Finding models needs an OpenAI API key. ChatGPT sign-in cannot list models.";
  }
  if (info.kind === "openai_compatible" && !info.base_url) {
    return "Save the endpoint's base URL to find the models it serves.";
  }
  if (
    !info.has_credential &&
    info.kind !== "ollama" &&
    info.kind !== "openai_compatible"
  ) {
    return "Save an API key to find the models this provider serves.";
  }
  return null;
}

function emptyHint(info: ProviderInfo): string {
  switch (info.kind) {
    case "ollama":
      return "No custom models yet. Add the models you have pulled, such as qwen3:0.6b for a first test.";
    case "openrouter":
      return "No custom models yet. Add one by its OpenRouter ID, such as anthropic/claude-sonnet-5.";
    case "openai_compatible":
      return "No custom models yet. Add each model this endpoint serves.";
    default:
      return "No custom models yet. Add one the provider ships before Tidebreak lists it.";
  }
}

function CustomModelRow({
  model,
  busy,
  onEdit,
  onRemove,
}: {
  model: CustomModelConfig;
  busy: boolean;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const name = modelName(model);
  return (
    <li className="flex items-start gap-3 border-b border-border-subtle px-3 py-2.5 last:border-b-0">
      <div className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="truncate text-sm font-medium">{name}</span>
        {model.display_name && (
          <span className="truncate font-mono text-xs text-muted-foreground">
            {model.id}
          </span>
        )}
        <span className="text-xs text-muted-foreground">
          {configuredModelFacts(model).join(" · ")}
        </span>
      </div>
      <div className="flex shrink-0 gap-1">
        <Button
          type="button"
          variant="ghost"
          size="xs"
          aria-label={`Edit ${name}`}
          disabled={busy}
          onClick={onEdit}
        >
          Edit
        </Button>
        <Button
          type="button"
          variant="ghost-destructive"
          size="xs"
          aria-label={`Remove ${name}`}
          disabled={busy}
          onClick={onRemove}
        >
          {busy ? "Removing…" : "Remove"}
        </Button>
      </div>
    </li>
  );
}
