import { describe, expect, it } from "vitest";

import type { CustomModelConfig, ModelInfo } from "../api";
import { providerInfoFixture } from "../stories/fixtures";
import {
  builtInModelIds,
  configFromDraft,
  draftFromConfig,
  draftFromDiscovered,
  emptyDraft,
  modelFacts,
  rowForSave,
  validateDraft,
} from "./customModels";

const none = new Set<string>();

describe("custom model drafts", () => {
  it("fills blank limits with the server's defaults and omits a blank name", () => {
    const config = configFromDraft(
      { ...emptyDraft(), id: "  local/model  ", displayName: "   " },
      ["low", "high"],
    );
    expect(config).toEqual({
      id: "local/model",
      context_window: 32_768,
      max_output_tokens: 4_096,
      input_modalities: ["text"],
      supports_reasoning: false,
      reasoning_efforts: [],
      supports_tools: true,
    });
    expect("display_name" in config).toBe(false);
  });

  it("keeps only the levels the route sends, in scale order", () => {
    const config = configFromDraft(
      {
        ...emptyDraft(),
        id: "m",
        supportsReasoning: true,
        reasoningEfforts: ["max", "none", "low"],
        contextWindow: "1,048,576",
      },
      ["low", "medium", "high", "xhigh", "max"],
    );
    expect(config.reasoning_efforts).toEqual(["low", "max"]);
    expect(config.context_window).toBe(1_048_576);
  });

  it("round-trips a saved row through the form", () => {
    const saved: CustomModelConfig = {
      id: "gpt-next",
      display_name: "GPT Next",
      context_window: 400_000,
      max_output_tokens: 64_000,
      input_modalities: ["text", "image"],
      supports_reasoning: true,
      reasoning_efforts: ["none", "high"],
      supports_tools: false,
    };
    expect(
      configFromDraft(draftFromConfig(saved), [
        "none",
        "low",
        "medium",
        "high",
        "xhigh",
        "max",
      ]),
    ).toEqual(saved);
  });

  it("starts a discovered model from what the provider reported, and only that", () => {
    const draft = draftFromDiscovered(
      {
        id: "vendor/model",
        context_window: 131_072,
        supports_reasoning: true,
        reasoning_efforts: ["none", "high"],
        built_in: false,
        added: false,
      },
      ["low", "high", "max"],
    );
    expect(draft).toEqual({
      id: "vendor/model",
      displayName: "",
      contextWindow: "131,072",
      // Not reported, so the form's default applies.
      maxOutputTokens: "",
      imageInput: false,
      supportsReasoning: true,
      // This route does not send "none".
      reasoningEfforts: ["high"],
      supportsTools: true,
    });
  });

  it("mirrors the server's limits field by field", () => {
    const check = (patch: Partial<ReturnType<typeof emptyDraft>>) =>
      validateDraft(
        { ...emptyDraft(), id: "model", ...patch },
        {
          takenIds: new Set(["taken"]),
          builtInIds: new Set(["built-in"]),
        },
      );
    expect(check({})).toEqual({});
    expect(check({ id: "" }).id).toMatch(/Enter the model ID/);
    expect(check({ id: "two words" }).id).toBe("A model ID has no spaces.");
    expect(check({ id: "built-in" }).id).toBe(
      "This model is already built in.",
    );
    expect(check({ id: "taken" }).id).toBe("You already added this model.");
    expect(check({ contextWindow: "1023" }).contextWindow).toBeDefined();
    expect(check({ contextWindow: "4000001" }).contextWindow).toBeDefined();
    expect(check({ contextWindow: "12.5" }).contextWindow).toBeDefined();
    expect(check({ maxOutputTokens: "0" }).maxOutputTokens).toBeDefined();
    // Blank context means the 32,768 default, which a larger output exceeds.
    expect(check({ maxOutputTokens: "40000" }).maxOutputTokens).toBe(
      "Max output cannot exceed the context window.",
    );
    expect(
      check({ contextWindow: "2048", maxOutputTokens: "" }).maxOutputTokens,
    ).toBe("Max output cannot exceed the context window.");
    expect(check({ displayName: "x".repeat(121) }).displayName).toBeDefined();
    expect(
      validateDraft(
        { ...emptyDraft(), id: "ok" },
        { takenIds: none, builtInIds: none },
      ),
    ).toEqual({});
  });

  it("drops a stored level the route no longer sends before saving the row again", () => {
    const stale: CustomModelConfig = {
      id: "grok-account-model",
      context_window: 500_000,
      max_output_tokens: 32_768,
      input_modalities: ["text"],
      supports_reasoning: true,
      reasoning_efforts: ["none", "low", "xhigh"],
      supports_tools: true,
    };
    expect(
      rowForSave(stale, ["low", "medium", "high", "xhigh"]).reasoning_efforts,
    ).toEqual(["low", "xhigh"]);
    expect(
      rowForSave({ ...stale, supports_reasoning: false }, ["low"])
        .reasoning_efforts,
    ).toEqual([]);
  });

  it("summarizes only the facts it knows", () => {
    expect(
      modelFacts({
        contextWindow: 1_000_000,
        maxOutputTokens: 128_000,
        imageInput: true,
        supportsReasoning: true,
        reasoningEfforts: ["max", "low", "high"],
        supportsTools: false,
      }),
    ).toEqual([
      "1M context",
      "128k output",
      "Images",
      "Reasoning, Low to Max",
      "No tools",
    ]);
    expect(modelFacts({})).toEqual([]);
    expect(modelFacts({ supportsReasoning: true })).toEqual(["Reasoning"]);
  });

  it("tells built-in models apart from the reader's own", () => {
    const row = (id: string): ModelInfo =>
      ({ id, provider: "openai" }) as ModelInfo;
    const info = providerInfoFixture("openai", {
      models: [
        {
          id: "gpt-next",
          context_window: 32_768,
          max_output_tokens: 4_096,
          input_modalities: ["text"],
          supports_reasoning: false,
          reasoning_efforts: [],
          supports_tools: true,
        },
      ],
    });
    expect(builtInModelIds(info, [row("gpt-6-sol"), row("gpt-next")])).toEqual(
      new Set(["gpt-6-sol"]),
    );
  });
});
