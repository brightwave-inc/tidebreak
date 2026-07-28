import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  firstAvailableModel,
  ModelMenu,
  ReasoningEffortMenu,
  reasoningEffortOptions,
  visibleModelGroups,
} from "./ModelMenu";
import { ProviderIcon } from "./ProviderIcons";
import type { ModelInfo } from "./api";

const MODELS: ModelInfo[] = [
  {
    key: "anthropic::claude-sonnet-4",
    id: "claude-sonnet-4",
    display_name: "Claude Sonnet 4",
    provider: "anthropic",
    context_window: 200_000,
    max_output_tokens: 64_000,
    input_modalities: ["text", "image"],
    supports_reasoning: true,
    reasoning_efforts: ["low", "medium", "high", "xhigh", "max"],
    multimodal: true,
    available: true,
  },
  {
    key: "openai::gpt-4o",
    id: "gpt-4o",
    display_name: "GPT-4o",
    provider: "openai",
    context_window: 128_000,
    max_output_tokens: 16_384,
    input_modalities: ["text", "image"],
    supports_reasoning: false,
    reasoning_efforts: [],
    multimodal: true,
    available: true,
  },
];

function triggerMarkup(value: string | null, defaultKey?: string | null): string {
  return renderToStaticMarkup(
    <ModelMenu
      models={MODELS}
      value={value}
      defaultKey={defaultKey ?? null}
      onChange={() => {}}
    />,
  );
}

describe("ModelMenu", () => {
  it("names the model the default resolves to", () => {
    const markup = triggerMarkup(null, "anthropic::claude-sonnet-4");
    expect(markup).toContain('aria-label="Model: Default (Claude Sonnet 4)"');
    expect(markup).toContain(">Default<");
  });

  it("promises nothing when the server names no default", () => {
    expect(triggerMarkup(null)).toContain('aria-label="Model: Default"');
  });

  it("labels the trigger with a selected model's display name", () => {
    const markup = triggerMarkup("openai::gpt-4o");
    expect(markup).toContain('aria-label="Model: GPT-4o"');
    expect(markup).toContain(">GPT-4o<");
  });

  it("labels the trigger with an unknown (custom) override verbatim", () => {
    const markup = triggerMarkup("local-model:latest");
    expect(markup).toContain('aria-label="Model: local-model:latest (unavailable)"');
    expect(markup).toContain(">local-model:latest (unavailable)<");
  });

  it("disables the trigger when asked", () => {
    const markup = renderToStaticMarkup(
      <ModelMenu models={MODELS} value={null} disabled onChange={() => {}} />,
    );
    expect(markup).toContain("disabled");
  });
});

describe("visibleModelGroups", () => {
  const withAvailability = (available: {
    anthropic: boolean;
    openai: boolean;
  }): ModelInfo[] =>
    MODELS.map((model) => ({
      ...model,
      available: available[model.provider as "anthropic" | "openai"],
    }));

  it("hides a provider whose models are all unavailable", () => {
    const groups = visibleModelGroups(
      withAvailability({ anthropic: true, openai: false }),
      null,
    );
    expect(groups.map((group) => group.provider)).toEqual(["anthropic"]);
  });

  it("keeps the group holding the current selection even when unavailable", () => {
    const groups = visibleModelGroups(
      withAvailability({ anthropic: true, openai: false }),
      "openai::gpt-4o",
    );
    expect(groups.map((group) => group.provider)).toEqual(["anthropic", "openai"]);
  });

  it("keeps a partially available provider intact", () => {
    const [sonnet] = MODELS;
    const haiku = {
      ...sonnet,
      key: "anthropic::claude-haiku" as const,
      id: "claude-haiku",
      available: false,
    };
    const anthropic = visibleModelGroups([sonnet, haiku], null).find(
      (group) => group.provider === "anthropic",
    );
    expect(anthropic?.models).toHaveLength(2);
  });
});

describe("firstAvailableModel", () => {
  it("lands in render order, not catalog order", () => {
    // OpenAI first in the catalog, but Anthropic renders first — the toggle
    // must land where the reader sees the check appear.
    const [sonnet, gpt] = MODELS;
    expect(firstAvailableModel([gpt, sonnet], null)?.key).toBe(
      "anthropic::claude-sonnet-4",
    );
  });

  it("is null when nothing can run", () => {
    const nothing = MODELS.map((model) => ({ ...model, available: false }));
    expect(firstAvailableModel(nothing, null)).toBeNull();
  });
});

describe("ProviderIcon", () => {
  it("gives each vendor its own mark", () => {
    const anthropic = renderToStaticMarkup(<ProviderIcon provider="anthropic" />);
    const openai = renderToStaticMarkup(<ProviderIcon provider="openai" />);
    const gemini = renderToStaticMarkup(<ProviderIcon provider="gemini" />);
    expect(new Set([anthropic, openai, gemini]).size).toBe(3);
  });

  it("keeps an open model's vendor mark whatever endpoint serves it", () => {
    const throughGateway = renderToStaticMarkup(
      <ProviderIcon provider="model_gateway" modelId="kimi-k2.5" />,
    );
    const throughCompatible = renderToStaticMarkup(
      <ProviderIcon provider="openai_compatible" modelId="accounts/x/models/kimi-k2" />,
    );
    const unbranded = renderToStaticMarkup(
      <ProviderIcon provider="model_gateway" modelId="some-model" />,
    );
    expect(throughGateway).toBe(throughCompatible);
    expect(throughGateway).not.toBe(unbranded);
  });
});

describe("reasoningEffortOptions", () => {
  it("offers exactly the levels the model accepts, in scale order", () => {
    expect(
      reasoningEffortOptions(["high", "low", "xhigh", "medium", "max"]).map(
        (option) => option.value,
      ),
    ).toEqual(["low", "medium", "high", "xhigh", "max"]);
    // The GPT-5 line adds an off level; generations before 5.6 stop at xhigh.
    expect(
      reasoningEffortOptions(["none", "low", "medium", "high", "xhigh"]).map(
        (option) => option.value,
      ),
    ).toEqual(["none", "low", "medium", "high", "xhigh"]);
    expect(reasoningEffortOptions([])).toEqual([]);
  });

  it("drops a level this build cannot label", () => {
    expect(
      reasoningEffortOptions(["low", "ultra" as never, "max"]).map(
        (option) => option.value,
      ),
    ).toEqual(["low", "max"]);
  });

  it("labels the off level as Off, distinct from the menu's Default entry", () => {
    expect(reasoningEffortOptions(["none"])[0].label).toBe("Off");
  });
});

describe("ReasoningEffortMenu", () => {
  const LEVELS = ["low", "medium", "high", "xhigh", "max"] as const;

  it("labels the trigger 'Default' when no effort is set", () => {
    const markup = renderToStaticMarkup(
      <ReasoningEffortMenu levels={LEVELS} value={null} onChange={() => {}} />,
    );
    expect(markup).toContain('aria-label="Reasoning effort: Default"');
    expect(markup).toContain(">Default<");
  });

  it("labels the trigger with the selected effort level", () => {
    const markup = renderToStaticMarkup(
      <ReasoningEffortMenu levels={LEVELS} value="high" onChange={() => {}} />,
    );
    expect(markup).toContain('aria-label="Reasoning effort: High"');
    expect(markup).toContain(">High<");
  });

  it("labels a level above high without falling back to its wire token", () => {
    const markup = renderToStaticMarkup(
      <ReasoningEffortMenu levels={LEVELS} value="xhigh" onChange={() => {}} />,
    );
    expect(markup).toContain('aria-label="Reasoning effort: X-high"');
  });

  it("still labels a stored level the current model no longer accepts", () => {
    const markup = renderToStaticMarkup(
      <ReasoningEffortMenu
        levels={["none", "low", "medium", "high", "xhigh"]}
        value="max"
        onChange={() => {}}
      />,
    );
    expect(markup).toContain('aria-label="Reasoning effort: Max"');
  });

  it("disables the trigger when asked", () => {
    const markup = renderToStaticMarkup(
      <ReasoningEffortMenu levels={LEVELS} value={null} disabled onChange={() => {}} />,
    );
    expect(markup).toContain("disabled");
  });
});
