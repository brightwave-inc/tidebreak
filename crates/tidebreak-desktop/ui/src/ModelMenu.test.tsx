import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  firstAvailableModel,
  matchingModels,
  ModelMenu,
  ModelToolCapabilityChip,
  notConnectedProviders,
  pickerGroupForSelection,
  pickerRailEntries,
  reasoningEffortOptions,
  visibleModelGroups,
} from "./ModelMenu";
import { ProviderIcon } from "./ProviderIcons";
import type { ModelInfo, ProviderInfo } from "./api";

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
    recommended: true,
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
    recommended: false,
  },
];

/** An available Anthropic model the catalog does not recommend. */
const HAIKU: ModelInfo = {
  ...MODELS[0],
  key: "anthropic::claude-haiku",
  id: "claude-haiku",
  display_name: "Claude Haiku",
  recommended: false,
};

function triggerMarkup(
  value: string | null,
  defaultKey?: string | null,
  models: ModelInfo[] = MODELS,
): string {
  return renderToStaticMarkup(
    <ModelMenu
      models={models}
      value={value}
      defaultKey={defaultKey ?? null}
      onSetUpProvider={() => {}}
      onChange={() => {}}
    />,
  );
}

describe("ModelMenu", () => {
  it("names the model the default resolves to", () => {
    const markup = triggerMarkup(null, "anthropic::claude-sonnet-4");
    expect(markup).toContain('aria-label="Model: Claude Sonnet 4"');
    expect(markup).toContain(">Claude Sonnet 4<");
    expect(markup).not.toContain(">Default<");
  });

  it("does not call an unconfigured install a default", () => {
    expect(triggerMarkup(null)).toContain('aria-label="No model selected"');
    expect(triggerMarkup(null)).toContain(">No model<");
    expect(triggerMarkup(null)).not.toContain(">Default<");
  });

  it("does not treat an unavailable boot default as selected", () => {
    const unavailable = MODELS.map((model) => ({ ...model, available: false }));
    const markup = triggerMarkup(
      null,
      "anthropic::claude-sonnet-4",
      unavailable,
    );
    expect(markup).toContain('aria-label="No model selected"');
    expect(markup).toContain(">No model<");
    expect(markup).not.toContain(">Default<");
  });

  it("labels the trigger with a selected model's display name", () => {
    const markup = triggerMarkup("openai::gpt-4o");
    expect(markup).toContain('aria-label="Model: GPT-4o"');
    expect(markup).toContain(">GPT-4o<");
  });

  it("labels the trigger with an unknown (custom) override verbatim", () => {
    const markup = triggerMarkup("local-model:latest");
    expect(markup).toContain(
      'aria-label="Model: local-model:latest (unavailable)"',
    );
    expect(markup).toContain(">local-model:latest (unavailable)<");
  });

  it("disables the trigger when asked", () => {
    const markup = renderToStaticMarkup(
      <ModelMenu
        models={MODELS}
        value={null}
        disabled
        onSetUpProvider={() => {}}
        onChange={() => {}}
      />,
    );
    expect(markup).toContain("disabled");
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
    expect(markup).toContain(">Conversation only<");
    expect(markup).toContain(
      "Conversation only. Function tools are unsupported or cannot yet be continued safely in Tidebreak.",
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
    expect(groups.map((group) => group.provider)).toEqual([
      "openai",
      "anthropic",
    ]);
    expect(groups[0].models.map((model) => model.key)).toEqual([
      "openai::gpt-4o",
    ]);
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

  it("lists every catalog row for a connected provider", () => {
    const anthropic = visibleModelGroups([MODELS[0], HAIKU], null).find(
      (group) => group.provider === "anthropic",
    );
    expect(anthropic?.models.map((model) => model.key)).toEqual([
      MODELS[0].key,
      HAIKU.key,
    ]);
  });

  it("splits a gateway catalog into vendor groups", () => {
    const viaGateway = (
      key: ModelInfo["key"],
      id: string,
      vendor: ModelInfo["vendor"],
    ) => ({
      ...MODELS[0],
      key,
      id,
      provider: "model_gateway" as const,
      vendor,
    });
    const groups = visibleModelGroups(
      [
        viaGateway("model_gateway::deepseek-v4", "deepseek-v4", null),
        viaGateway(
          "model_gateway::claude-sonnet-4",
          "claude-sonnet-4",
          "anthropic",
        ),
        viaGateway("model_gateway::gpt-5", "gpt-5", "openai"),
        viaGateway("model_gateway::glm-5.2", "glm-5.2", null),
        viaGateway("model_gateway::mystery-model", "mystery-model", null),
      ],
      null,
    );
    expect(groups.map((group) => [group.id, group.label])).toEqual([
      ["model_gateway/openai", "OpenAI"],
      ["model_gateway/anthropic", "Anthropic"],
      ["model_gateway/deepseek", "DeepSeek"],
      ["model_gateway/glm", "Z.ai"],
      ["model_gateway", "Model Gateway"],
    ]);
    expect(
      groups.map((group) => group.models.map((model) => model.id)),
    ).toEqual([
      ["gpt-5"],
      ["claude-sonnet-4"],
      ["deepseek-v4"],
      ["glm-5.2"],
      ["mystery-model"],
    ]);
  });
});

describe("matchingModels", () => {
  const gatewayClaude: ModelInfo = {
    ...MODELS[0],
    key: "model_gateway::claude-sonnet-4",
    provider: "model_gateway",
    vendor: "anthropic",
  };

  it("matches model, serving-provider, and vendor text across groups", () => {
    const groups = visibleModelGroups([MODELS[1], gatewayClaude], null);
    expect(matchingModels(groups, "gpt").map((model) => model.key)).toEqual([
      "openai::gpt-4o",
    ]);
    expect(matchingModels(groups, "gateway").map((model) => model.key)).toEqual(
      [gatewayClaude.key],
    );
    expect(
      matchingModels(groups, "anthropic").map((model) => model.key),
    ).toEqual([gatewayClaude.key]);
  });
});

describe("notConnectedProviders", () => {
  const credentialed = (kind: ProviderInfo["kind"]): ProviderInfo => ({
    kind,
    enabled: true,
    has_credential: true,
    models: [],
  });

  it("offers setup for a provider whose catalog rows cannot run", () => {
    const gemini: ModelInfo[] = [
      { ...MODELS[0], key: "gemini::a", provider: "gemini", available: false },
      { ...MODELS[0], key: "gemini::b", provider: "gemini", available: false },
    ];
    expect(
      notConnectedProviders(
        [...MODELS, ...gemini],
        [credentialed("anthropic")],
      ),
    ).toEqual([{ provider: "gemini", modelCount: 2 }]);
  });

  it("leaves out the gateway, whose models are policy rather than a key", () => {
    const gateway: ModelInfo = {
      ...MODELS[0],
      key: "model_gateway::claude",
      provider: "model_gateway",
      available: false,
    };
    expect(notConnectedProviders([gateway], [])).toEqual([]);
  });

  it("offers no setup at all on a managed profile", () => {
    // Managed policy refuses every provider credential write, so a row whose
    // only action is "Set up" can lead nowhere.
    const gemini: ModelInfo[] = [
      { ...MODELS[0], key: "gemini::a", provider: "gemini", available: false },
    ];
    expect(notConnectedProviders([...MODELS, ...gemini], [], true)).toEqual([]);
  });
});

describe("pickerRailEntries", () => {
  const gemini: ModelInfo = {
    ...MODELS[0],
    key: "gemini::a",
    provider: "gemini",
    available: false,
    recommended: true,
  };

  it("lists connected providers and ones still to set up", () => {
    const rail = pickerRailEntries([...MODELS, gemini], [], null);
    expect(rail.map((entry) => [entry.provider, entry.connected])).toEqual([
      ["openai", true],
      ["anthropic", true],
      ["gemini", false],
    ]);
  });

  it("omits unconfigured providers on a managed profile", () => {
    const rail = pickerRailEntries([...MODELS, gemini], [], null, true);
    expect(rail.map((entry) => entry.provider)).toEqual([
      "openai",
      "anthropic",
    ]);
  });

  it("places OpenRouter on the rail after Together", () => {
    const together: ModelInfo = {
      ...MODELS[0],
      key: "together::kimi",
      id: "kimi",
      provider: "together",
      available: true,
    };
    const openrouter: ModelInfo = {
      ...MODELS[0],
      key: "openrouter::anthropic/claude-sonnet-4",
      id: "anthropic/claude-sonnet-4",
      provider: "openrouter",
      available: false,
    };
    const rail = pickerRailEntries([...MODELS, together, openrouter], [], null);
    expect(rail.map((entry) => [entry.provider, entry.connected])).toEqual([
      ["openai", true],
      ["anthropic", true],
      ["together", true],
      ["openrouter", false],
    ]);
  });

  it("gives each gateway vendor group its own rail tab", () => {
    const viaGateway = (
      key: ModelInfo["key"],
      id: string,
      vendor: ModelInfo["vendor"],
    ) => ({
      ...MODELS[0],
      key,
      id,
      provider: "model_gateway" as const,
      vendor,
    });
    const rail = pickerRailEntries(
      [
        viaGateway(
          "model_gateway::claude-sonnet-4",
          "claude-sonnet-4",
          "anthropic",
        ),
        viaGateway("model_gateway::gpt-5", "gpt-5", "openai"),
        viaGateway("model_gateway::deepseek-v4", "deepseek-v4", null),
      ],
      [],
      null,
    );
    expect(rail.map((entry) => [entry.id, entry.connected])).toEqual([
      ["model_gateway/openai", true],
      ["model_gateway/anthropic", true],
      ["model_gateway/deepseek", true],
    ]);
  });

  it("does not offer setup for a provider a gateway group already serves", () => {
    const directClaude: ModelInfo = {
      ...MODELS[0],
      available: false,
    };
    const gatewayClaude: ModelInfo = {
      ...MODELS[0],
      key: "model_gateway::claude-sonnet-4",
      provider: "model_gateway",
      vendor: "anthropic",
    };
    const rail = pickerRailEntries([directClaude, gatewayClaude], [], null);
    expect(rail.map((entry) => entry.id)).toEqual(["model_gateway/anthropic"]);
  });
});

describe("pickerGroupForSelection", () => {
  const groups = [{ id: "openai" }, { id: "anthropic" }];

  it("opens on the selected model's group", () => {
    expect(pickerGroupForSelection(groups, MODELS[0])).toBe("anthropic");
  });

  it("opens on a gateway model's vendor group", () => {
    const gatewayClaude: ModelInfo = {
      ...MODELS[0],
      key: "model_gateway::claude-sonnet-4",
      provider: "model_gateway",
      vendor: "anthropic",
    };
    expect(
      pickerGroupForSelection(
        [{ id: "model_gateway/anthropic" }, { id: "model_gateway/openai" }],
        gatewayClaude,
      ),
    ).toBe("model_gateway/anthropic");
  });

  it("falls back to the first visible group", () => {
    expect(pickerGroupForSelection(groups, null)).toBe("openai");
    expect(
      pickerGroupForSelection(groups, {
        ...MODELS[0],
        provider: "gemini",
      }),
    ).toBe("openai");
  });

  it("is null when nothing is listed", () => {
    expect(pickerGroupForSelection([], MODELS[0])).toBeNull();
  });
});

describe("firstAvailableModel", () => {
  it("lands in render order", () => {
    const [sonnet, gpt] = MODELS;
    expect(firstAvailableModel([gpt, sonnet], null)?.key).toBe(
      "openai::gpt-4o",
    );
  });

  it("is null when nothing can run", () => {
    const nothing = MODELS.map((model) => ({ ...model, available: false }));
    expect(firstAvailableModel(nothing, null)).toBeNull();
  });
});

describe("ProviderIcon", () => {
  it("draws the Grok/xAI ribbon, not a stroke X", () => {
    const markup = renderToStaticMarkup(<ProviderIcon provider="xai" />);
    expect(markup).toContain("M9.269");
    expect(markup).not.toContain("strokeWidth");
    expect(markup).not.toContain("M5 4l14 16");
  });

  it("draws the Ollama llama and the OpenRouter mark", () => {
    const ollama = renderToStaticMarkup(<ProviderIcon provider="ollama" />);
    expect(ollama).toContain("M16.361 10.26");
    expect(ollama).not.toContain("lucide");

    const openrouter = renderToStaticMarkup(
      <ProviderIcon provider="openrouter" />,
    );
    expect(openrouter).toContain("#7624F4");
    expect(openrouter).toContain("M303.9475 17.1993");
  });

  it("keeps an open model's vendor mark whatever endpoint serves it", () => {
    const throughGateway = renderToStaticMarkup(
      <ProviderIcon provider="model_gateway" modelId="kimi-k2.5" />,
    );
    const throughCompatible = renderToStaticMarkup(
      <ProviderIcon
        provider="openai_compatible"
        modelId="accounts/x/models/kimi-k2"
      />,
    );
    const unbranded = renderToStaticMarkup(
      <ProviderIcon provider="model_gateway" modelId="some-model" />,
    );
    expect(throughGateway).toBe(throughCompatible);
    expect(throughGateway).not.toBe(unbranded);
  });

  it("uses a route-neutral mark for an unmatched gateway model", () => {
    const gateway = renderToStaticMarkup(
      <ProviderIcon provider="model_gateway" />,
    );
    expect(gateway).toContain("lucide-network");
    expect(gateway).not.toContain("lucide-lock-open");
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
        onSetUpProvider={() => {}}
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
