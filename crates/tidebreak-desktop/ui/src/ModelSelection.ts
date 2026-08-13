import type {
  ModelInfo,
  ModelRole,
  ModelRoleInfo,
  ModelSelectionKey,
  ProviderKind,
} from "./api";

/**
 * Resolve a stored selection against the typed catalog.
 *
 * New values match only their provider-qualified key. Old bare ids are
 * accepted only when exactly one direct-vendor row owns it. A hosted mirror
 * would carry `vendor` and never steal a legacy selection from the original
 * direct route.
 */
export function modelForSelection(
  models: ModelInfo[],
  value: string | null,
): ModelInfo | null {
  if (!value) return null;
  const exact = models.find((model) => model.key === value);
  if (exact) return exact;
  if (value.includes("::")) return null;
  const legacy = models.filter(
    (model) => model.id === value && model.vendor === null,
  );
  return legacy.length === 1 ? legacy[0] : null;
}

/** Canonical provider-qualified key for a current or legacy selection. */
export function canonicalModelSelection(
  models: ModelInfo[],
  value: string | null,
): ModelSelectionKey | null {
  return modelForSelection(models, value)?.key ?? null;
}

/**
 * The label of a selected model when it cannot read images, or `null`.
 *
 * A selection the renderer cannot resolve — none made, or one whose provider
 * is gone — follows the global default, which the renderer does not know; the
 * server still refuses such a turn, so the composer stays quiet rather than
 * guessing at a name it would have to print.
 */
export function textOnlyModelLabel(
  models: ModelInfo[],
  selection: string | null,
): string | null {
  const model = modelForSelection(models, selection);
  return model && !model.multimodal ? model.display_name : null;
}

/** Provider display label shared by settings and the composer. */
export function providerLabel(provider: ProviderKind): string {
  switch (provider) {
    case "anthropic":
      return "Anthropic";
    case "openai":
      return "OpenAI";
    case "xai":
      return "xAI";
    case "gemini":
      return "Google Gemini";
    case "fireworks":
      return "Fireworks AI";
    case "together":
      return "Together AI";
    case "ollama":
      return "Ollama";
    case "openai_compatible":
      return "OpenAI-compatible";
    case "model_gateway":
      return "Model Gateway";
  }
}

/**
 * What `role` resolves to right now, as the server reports it.
 *
 * `null` when the server could not name a model for the role, which a client
 * must present as "nothing" rather than guessing at one.
 */
export function resolvedRoleKey(
  roles: ModelRoleInfo[],
  role: ModelRole,
): string | null {
  return roles.find((entry) => entry.role === role)?.resolved_key ?? null;
}
