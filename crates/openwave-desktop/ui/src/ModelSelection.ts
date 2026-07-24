import type {
  ModelInfo,
  ModelSelectionKey,
  ProviderKind,
} from "./api";

/**
 * Resolve a stored selection against the typed catalog.
 *
 * New values match only their provider-qualified key. Old bare ids are
 * accepted only when the catalog has exactly one owner, so migration can never
 * silently change providers.
 */
export function modelForSelection(
  models: ModelInfo[],
  value: string | null,
): ModelInfo | null {
  if (!value) return null;
  const exact = models.find((model) => model.key === value);
  if (exact) return exact;
  if (value.includes("::")) return null;
  const legacy = models.filter((model) => model.id === value);
  return legacy.length === 1 ? legacy[0] : null;
}

/** Canonical provider-qualified key for a current or legacy selection. */
export function canonicalModelSelection(
  models: ModelInfo[],
  value: string | null,
): ModelSelectionKey | null {
  return modelForSelection(models, value)?.key ?? null;
}

/** Provider display label shared by settings and the composer. */
export function providerLabel(provider: ProviderKind): string {
  switch (provider) {
    case "anthropic":
      return "Anthropic";
    case "openai":
      return "OpenAI";
    case "openai_compatible":
      return "OpenAI-compatible";
  }
}
