import type {
  CustomModelConfig,
  DiscoveredModel,
  ModelInfo,
  ProviderInfo,
  ReasoningEffort,
} from "../api";
import { reasoningEffortOptions } from "../ModelMenu";

/**
 * Custom model rows, as the Providers settings edit them.
 *
 * The server validates every row when it is saved. These helpers mirror its
 * limits so the form can point at the field that is wrong before the reader
 * submits, and so a blank limit becomes the same default the server applies.
 */

/** The context window a row gets when the reader leaves it blank. */
export const DEFAULT_CONTEXT_WINDOW = 32_768;
/** The output cap a row gets when the reader leaves it blank. */
export const DEFAULT_MAX_OUTPUT_TOKENS = 4_096;
export const MIN_CONTEXT_WINDOW = 1_024;
export const MAX_CONTEXT_WINDOW = 4_000_000;
export const MAX_MODEL_ID_CHARS = 240;
export const MAX_DISPLAY_NAME_CHARS = 120;
/** The most custom rows one provider keeps. */
export const MAX_CUSTOM_MODELS = 64;

/**
 * One row as the form edits it. The limits stay text until the row is saved,
 * so a half-typed number is never coerced and a blank one can fall back to
 * the default.
 */
export type ModelDraft = {
  id: string;
  displayName: string;
  contextWindow: string;
  maxOutputTokens: string;
  imageInput: boolean;
  supportsReasoning: boolean;
  reasoningEfforts: ReasoningEffort[];
  supportsTools: boolean;
};

export type ModelDraftField =
  | "id"
  | "displayName"
  | "contextWindow"
  | "maxOutputTokens";

export type ModelDraftErrors = Partial<Record<ModelDraftField, string>>;

export function emptyDraft(): ModelDraft {
  return {
    id: "",
    displayName: "",
    contextWindow: "",
    maxOutputTokens: "",
    imageInput: false,
    supportsReasoning: false,
    reasoningEfforts: [],
    supportsTools: true,
  };
}

/** A token count as the form shows it, with thousands separators. */
function tokenText(tokens: number): string {
  return tokens.toLocaleString("en-US");
}

export function draftFromConfig(model: CustomModelConfig): ModelDraft {
  return {
    id: model.id,
    displayName: model.display_name ?? "",
    contextWindow: tokenText(model.context_window),
    maxOutputTokens: tokenText(model.max_output_tokens),
    imageInput: model.input_modalities.includes("image"),
    supportsReasoning: model.supports_reasoning,
    reasoningEfforts: [...model.reasoning_efforts],
    // The server leaves the flag out while tools are on.
    supportsTools: model.supports_tools !== false,
  };
}

/**
 * A draft from what a provider reported. Anything it did not report takes
 * the form's own default: a blank limit, no images, no reasoning, tools on.
 */
export function draftFromDiscovered(
  model: DiscoveredModel,
  accepted: readonly ReasoningEffort[],
): ModelDraft {
  const supportsReasoning = model.supports_reasoning === true;
  return {
    id: model.id,
    displayName: model.display_name ?? "",
    contextWindow:
      model.context_window === undefined ? "" : tokenText(model.context_window),
    maxOutputTokens:
      model.max_output_tokens === undefined
        ? ""
        : tokenText(model.max_output_tokens),
    imageInput: model.image_input === true,
    supportsReasoning,
    reasoningEfforts: supportsReasoning
      ? (model.reasoning_efforts ?? []).filter((effort) =>
          accepted.includes(effort),
        )
      : [],
    supportsTools: model.supports_tools !== false,
  };
}

function parseTokens(value: string): number | null {
  const trimmed = value.replace(/[,_\s]/g, "");
  if (!/^\d+$/.test(trimmed)) return null;
  return Number(trimmed);
}

/** The limit a field stands for: its number, or the default when blank. */
function tokensOrDefault(value: string, fallback: number): number | null {
  return value.trim() === "" ? fallback : parseTokens(value);
}

/**
 * What is wrong with a draft, by field. Empty when the row can be saved.
 *
 * `takenIds` are the other custom rows on this provider, and `builtInIds` the
 * provider's built-in models: a custom row may repeat neither.
 */
export function validateDraft(
  draft: ModelDraft,
  {
    takenIds,
    builtInIds,
  }: { takenIds: ReadonlySet<string>; builtInIds: ReadonlySet<string> },
): ModelDraftErrors {
  const errors: ModelDraftErrors = {};
  const id = draft.id.trim();
  if (!id) {
    errors.id = "Enter the model ID exactly as the provider's API spells it.";
  } else if (/\s/.test(id)) {
    errors.id = "A model ID has no spaces.";
  } else if (id.length > MAX_MODEL_ID_CHARS) {
    errors.id = `A model ID is at most ${MAX_MODEL_ID_CHARS} characters.`;
  } else if (builtInIds.has(id)) {
    errors.id = "This model is already built in.";
  } else if (takenIds.has(id)) {
    errors.id = "You already added this model.";
  }

  if (draft.displayName.trim().length > MAX_DISPLAY_NAME_CHARS) {
    errors.displayName = `A display name is at most ${MAX_DISPLAY_NAME_CHARS} characters.`;
  }

  const context = tokensOrDefault(draft.contextWindow, DEFAULT_CONTEXT_WINDOW);
  if (
    context === null ||
    context < MIN_CONTEXT_WINDOW ||
    context > MAX_CONTEXT_WINDOW
  ) {
    errors.contextWindow = `Enter a whole number from ${MIN_CONTEXT_WINDOW.toLocaleString("en-US")} to ${MAX_CONTEXT_WINDOW.toLocaleString("en-US")}.`;
  }

  const output = tokensOrDefault(
    draft.maxOutputTokens,
    DEFAULT_MAX_OUTPUT_TOKENS,
  );
  if (output === null || output < 1) {
    errors.maxOutputTokens = "Enter a whole number above zero.";
  } else if (context !== null && !errors.contextWindow && output > context) {
    errors.maxOutputTokens = "Max output cannot exceed the context window.";
  }
  return errors;
}

/**
 * The row to save. Call only on a draft with no errors. Reasoning levels are
 * kept to the ones this provider's route sends, in scale order.
 */
export function configFromDraft(
  draft: ModelDraft,
  accepted: readonly ReasoningEffort[],
): CustomModelConfig {
  const displayName = draft.displayName.trim();
  return {
    id: draft.id.trim(),
    // Omitted rather than empty, which is how the server represents an unset
    // display name.
    ...(displayName ? { display_name: displayName } : {}),
    context_window:
      tokensOrDefault(draft.contextWindow, DEFAULT_CONTEXT_WINDOW) ??
      DEFAULT_CONTEXT_WINDOW,
    max_output_tokens:
      tokensOrDefault(draft.maxOutputTokens, DEFAULT_MAX_OUTPUT_TOKENS) ??
      DEFAULT_MAX_OUTPUT_TOKENS,
    input_modalities: draft.imageInput ? ["text", "image"] : ["text"],
    supports_reasoning: draft.supportsReasoning,
    reasoning_efforts: draft.supportsReasoning
      ? accepted.filter((effort) => draft.reasoningEfforts.includes(effort))
      : [],
    ...toolsFlag(draft.supportsTools),
  };
}

/**
 * The tools flag as a saved row carries it: only when tools are off. On is
 * the server's default, and leaving it out lets a server that predates the
 * flag accept the row.
 */
function toolsFlag(
  supportsTools: boolean,
): Pick<CustomModelConfig, "supports_tools"> {
  return supportsTools ? {} : { supports_tools: false };
}

/**
 * A stored row as the next save sends it. A row saved by an older build may
 * list a reasoning level this provider no longer accepts; sending it back
 * would fail the whole save, so it is dropped here. A server that predates
 * the accepted list sends none, and then the row's levels go back as they
 * are.
 */
export function rowForSave(
  model: CustomModelConfig,
  accepted: readonly ReasoningEffort[] | undefined,
): CustomModelConfig {
  const { supports_tools: supportsTools, ...row } = model;
  return {
    ...row,
    reasoning_efforts: !model.supports_reasoning
      ? []
      : accepted === undefined
        ? model.reasoning_efforts
        : accepted.filter((effort) => model.reasoning_efforts.includes(effort)),
    ...toolsFlag(supportsTools !== false),
  };
}

/** A short range for a list of reasoning levels: "Low", or "Low to Max". */
export function effortRangeLabel(efforts: readonly ReasoningEffort[]): string {
  const options = reasoningEffortOptions(efforts);
  if (options.length === 0) return "";
  if (options.length === 1) return options[0].label;
  return `${options[0].label} to ${options[options.length - 1].label}`;
}

type ModelFactsInput = {
  contextWindow?: number;
  maxOutputTokens?: number;
  imageInput?: boolean;
  supportsReasoning?: boolean;
  reasoningEfforts?: readonly ReasoningEffort[];
  supportsTools?: boolean;
};

/**
 * A limit at a glance: "128k", or "1M" for any window that rounds to a whole
 * million, so a 1,048,576-token window reads the way providers name it.
 */
export function compactTokens(tokens: number): string {
  if (tokens >= 1_000_000) return `${Math.round(tokens / 100_000) / 10}M`;
  if (tokens >= 1_000) return `${Math.round(tokens / 1_000)}k`;
  return `${tokens}`;
}

/**
 * The one-line summary under a model's name. Unknown facts are left out
 * rather than guessed.
 */
export function modelFacts(input: ModelFactsInput): string[] {
  const facts: string[] = [];
  if (input.contextWindow !== undefined) {
    facts.push(`${compactTokens(input.contextWindow)} context`);
  }
  if (input.maxOutputTokens !== undefined) {
    facts.push(`${compactTokens(input.maxOutputTokens)} output`);
  }
  if (input.imageInput) facts.push("Images");
  if (input.supportsReasoning) {
    const range = effortRangeLabel(input.reasoningEfforts ?? []);
    facts.push(range ? `Reasoning, ${range}` : "Reasoning");
  }
  if (input.supportsTools === false) facts.push("No tools");
  return facts;
}

export function configuredModelFacts(model: CustomModelConfig): string[] {
  return modelFacts({
    contextWindow: model.context_window,
    maxOutputTokens: model.max_output_tokens,
    imageInput: model.input_modalities.includes("image"),
    supportsReasoning: model.supports_reasoning,
    reasoningEfforts: model.reasoning_efforts,
    supportsTools: model.supports_tools,
  });
}

export function discoveredModelFacts(model: DiscoveredModel): string[] {
  return modelFacts({
    contextWindow: model.context_window,
    maxOutputTokens: model.max_output_tokens,
    imageInput: model.image_input,
    supportsReasoning: model.supports_reasoning,
    reasoningEfforts: model.reasoning_efforts,
    supportsTools: model.supports_tools,
  });
}

/**
 * The ids of a provider's built-in models: the catalog rows it lists that
 * are not the reader's own. A saved row the catalog has since built in is no
 * longer among `info.models`, so its id counts as built in here.
 */
export function builtInModelIds(
  info: ProviderInfo,
  catalogModels: readonly ModelInfo[],
): Set<string> {
  const custom = new Set(info.models.map((model) => model.id));
  return new Set(
    catalogModels
      .filter((model) => model.provider === info.kind && !custom.has(model.id))
      .map((model) => model.id),
  );
}
