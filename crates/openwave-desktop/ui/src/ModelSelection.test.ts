import { describe, expect, it } from "vitest";
import type { ModelInfo } from "./api";
import {
  canonicalModelSelection,
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
  reasoning_efforts: [],
  multimodal: false,
  available: true,
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
];

describe("typed model selection", () => {
  it("resolves provider-qualified keys exactly", () => {
    expect(modelForSelection(models, "openai_compatible::shared")?.provider).toBe(
      "openai_compatible",
    );
    expect(modelForSelection(models, "anthropic::shared")).toBeNull();
  });

  it("migrates only unambiguous legacy ids", () => {
    expect(canonicalModelSelection(models, "unique")).toBe("anthropic::unique");
    expect(canonicalModelSelection(models, "shared")).toBeNull();
  });

  it("uses product-facing provider labels", () => {
    expect(providerLabel("openai")).toBe("OpenAI");
    expect(providerLabel("gemini")).toBe("Google Gemini");
    expect(providerLabel("openai_compatible")).toBe("OpenAI-compatible");
  });
});
