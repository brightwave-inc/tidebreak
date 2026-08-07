import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  firstAvailableModel,
  ModelMenu,
  ModelToolCapabilityChip,
  ModelVerificationChip,
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
    vendor: null,
    verification: "verified",
    context_window: 200_000,
    max_output_tokens: 64_000,
    input_modalities: ["text", "image"],
    supports_reasoning: true,
    supports_tools: true,
    supports_structured_output: true,
    reasoning_efforts: ["low", "medium", "high", "xhigh", "max"],
    multimodal: true,
    available: true,
  },
  {
    key: "openai::gpt-4o",
    id: "gpt-4o",
    display_name: "GPT-4o",
    provider: "openai",
    vendor: null,
    verification: "verified",
    context_window: 128_000,
    max_output_tokens: 16_384,
    input_modalities: ["text", "image"],
    supports_reasoning: false,
    supports_tools: true,
    supports_structured_output: true,
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

  it("labels an unverified model with its explanation", () => {
    const markup = renderToStaticMarkup(
      <ModelVerificationChip
        model={{
          ...MODELS[1],
          key: "openai_compatible::local-model",
          id: "local-model",
          display_name: "Local Model",
          provider: "openai_compatible",
          vendor: null,
          verification: "unverified",
        }}
      />,
    );
    expect(markup).toContain("Unverified");
    expect(markup).toContain(
      "Unverified. OpenWave hasn&#x27;t verified tool-calling and streaming for this model; issues are likely the model or provider, not the app",
    );
  });

  it("explains chat-only routing without blaming provider capability", () => {
    const markup = renderToStaticMarkup(
      <ModelToolCapabilityChip
        model={{
          ...MODELS[1],
          key: "together::moonshotai/Kimi-K3",
          id: "moonshotai/Kimi-K3",
          display_name: "Kimi K3",
          provider: "together",
          verification: "unverified",
          supports_tools: false,
        }}
      />,
    );
    expect(markup).toContain(">Chat only<");
    expect(markup).toContain(
      "Chat only. Function tools are unsupported or cannot yet be continued safely in OpenWave.",
    );
    expect(markup).not.toContain("hosted model does not support");
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

describe("model marks", () => {
  it("brands a gateway model by the vendor it matched, not by the gateway", () => {
    const gatewayClaude: ModelInfo = {
      ...MODELS[0],
      key: "model_gateway::claude-sonnet-4",
      provider: "model_gateway",
      vendor: "anthropic",
    };
    const trigger = renderToStaticMarkup(
      <ModelMenu
        models={[gatewayClaude]}
        value={gatewayClaude.key}
        defaultKey={null}
        onChange={() => {}}
      />,
    );
    expect(trigger).toContain(
      renderToStaticMarkup(
        <ProviderIcon
          provider="anthropic"
          modelId={gatewayClaude.id}
          className="size-4"
        />,
      ),
    );
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
