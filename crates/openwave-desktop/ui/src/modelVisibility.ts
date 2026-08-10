import type { ModelInfo } from "./api/types";

/**
 * Per-model deviations from the catalog's `recommended` flag, keyed by
 * `ModelInfo.key`. Deviations only: a model with no entry uses its catalog
 * default, which is what lets a catalog refresh change a default without
 * touching anyone's choices.
 */
export type ModelVisibilityOverrides = Record<string, "show" | "hide">;

/**
 * Whether a picker shows this model by default: the recommended flag, flipped
 * by an override when one exists.
 */
export function isModelVisible(
  model: Pick<ModelInfo, "key" | "recommended">,
  overrides: ModelVisibilityOverrides,
): boolean {
  const override = overrides[model.key];
  if (override === "show") return true;
  if (override === "hide") return false;
  return model.recommended;
}
