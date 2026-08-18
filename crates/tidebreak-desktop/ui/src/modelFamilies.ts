import type { ProviderKind } from "./api";

/**
 * Vendor identity derived from a model id, shared by the chat picker and the
 * code-mode picker so a model reads as the same vendor everywhere.
 */

/**
 * Open-model families recognizable from a model id, mirroring the id
 * heuristics `ProviderIcon` brands rows with. `match` doubles as the
 * `modelId` an icon renders the family mark for.
 */
export const MODEL_ID_FAMILIES: { match: string; label: string }[] = [
  { match: "deepseek", label: "DeepSeek" },
  { match: "glm", label: "Z.ai" },
  { match: "kimi", label: "Moonshot AI" },
  { match: "minimax", label: "MiniMax" },
  { match: "qwen", label: "Qwen" },
  { match: "nemotron", label: "NVIDIA" },
  { match: "gemma", label: "Gemma" },
];

/**
 * First-party vendors recognizable from a model id alone. Used where a
 * catalog carries bare ids with no vendor field — a harness's own model
 * list, or a gateway row the server matched no curated model for.
 */
const VENDOR_ID_PATTERNS: { match: RegExp; vendor: ProviderKind }[] = [
  { match: /claude|sonnet|opus|haiku/, vendor: "anthropic" },
  { match: /gpt|codex|^o\d/, vendor: "openai" },
  { match: /grok/, vendor: "xai" },
  { match: /gemini/, vendor: "gemini" },
];

/** The first-party vendor a bare model id names, or `null`. */
export function vendorForModelId(id: string): ProviderKind | null {
  const leaf = (id.split("/").pop() ?? id).toLocaleLowerCase();
  return (
    VENDOR_ID_PATTERNS.find((entry) => entry.match.test(leaf))?.vendor ?? null
  );
}

/** The open-model family a model id names, or `null`. */
export function familyForModelId(
  id: string,
): { match: string; label: string } | null {
  const needle = id.toLocaleLowerCase();
  return (
    MODEL_ID_FAMILIES.find((entry) => needle.includes(entry.match)) ?? null
  );
}
