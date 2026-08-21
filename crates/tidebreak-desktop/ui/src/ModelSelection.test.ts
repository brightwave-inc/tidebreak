import { describe, expect, it } from "vitest";
import type { ModelInfo } from "./api";
import {
  canonicalModelSelection,
  modelForChat,
  modelForSelection,
  providerLabel,
} from "./ModelSelection";

const base = {
  display_name: "Shared",
  verification: "verified" as const,
  context_window: 32_000,
  max_output_tokens: 4_000,
  input_modalities: ["text"] as const,
  supports_reasoning: false,
  supports_tools: true,
  supports_structured_output: true,
  reasoning_efforts: [],
  multimodal: false,
  available: true,
  recommended: false,
};

const models: ModelInfo[] = [
  {
    ...base,
    key: "openai::shared",
    id: "shared",
    provider: "openai",
    vendor: null,
    input_modalities: ["text"],
  },
  {
    ...base,
    key: "openai_compatible::shared",
    id: "shared",
    provider: "openai_compatible",
    vendor: null,
    verification: "unverified",
    input_modalities: ["text"],
  },
  {
    ...base,
    key: "anthropic::unique",
    id: "unique",
    provider: "anthropic",
    vendor: null,
    input_modalities: ["text"],
  },
  {
    ...base,
    key: "model_gateway::unique",
    id: "unique",
    provider: "model_gateway",
    vendor: "anthropic",
    verification: "unverified",
    input_modalities: ["text"],
  },
];

describe("typed model selection", () => {
  it("resolves provider-qualified keys exactly", () => {
    expect(
      modelForSelection(models, "openai_compatible::shared")?.provider,
    ).toBe("openai_compatible");
    expect(modelForSelection(models, "anthropic::shared")).toBeNull();
  });

  it("migrates only unambiguous legacy ids", () => {
    expect(canonicalModelSelection(models, "unique")).toBe("anthropic::unique");
    expect(canonicalModelSelection(models, "shared")).toBeNull();
  });

  it("follows the catalog default when the chat has no override", () => {
    // A null chat.model is "use the default", not "no model" — the picker
    // already names GPT-5.6 Sol that way, and the context meter has to
    // resolve the same row or it has no window to read against.
    expect(modelForChat(models, null, "anthropic::unique")?.key).toBe(
      "anthropic::unique",
    );
    expect(
      modelForChat(models, "openai::shared", "anthropic::unique")?.key,
    ).toBe("openai::shared");
    expect(
      modelForChat(models, "missing::model", "anthropic::unique"),
    ).toBeNull();
  });

  it("uses product-facing provider labels", () => {
    expect(providerLabel("openai")).toBe("OpenAI");
    expect(providerLabel("xai")).toBe("xAI");
    expect(providerLabel("gemini")).toBe("Google Gemini");
    expect(providerLabel("fireworks")).toBe("Fireworks AI");
    expect(providerLabel("together")).toBe("Together AI");
    expect(providerLabel("openrouter")).toBe("OpenRouter");
    expect(providerLabel("ollama")).toBe("Ollama");
    expect(providerLabel("openai_compatible")).toBe("OpenAI-compatible");
  });
});
