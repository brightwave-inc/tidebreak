import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ModelCapabilities,
  ModelMenu,
  ProviderIcon,
  ReasoningEffortMenu,
  formatContextWindow,
  reasoningEffortOptions,
} from "./ModelMenu";
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

function triggerMarkup(value: string | null): string {
  return renderToStaticMarkup(
    <ModelMenu models={MODELS} value={value} onChange={() => {}} />,
  );
}

describe("ModelMenu", () => {
  it("labels the trigger 'Default' when no override is set", () => {
    const markup = triggerMarkup(null);
    expect(markup).toContain('aria-label="Model: Default"');
    expect(markup).toContain(">Default<");
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
      <ModelMenu models={MODELS} value={null} disabled onChange={() => {}} />,
    );
    expect(markup).toContain("disabled");
  });
});

describe("formatContextWindow", () => {
  it("renders thousands with a K suffix", () => {
    expect(formatContextWindow(128_000)).toBe("128K");
    expect(formatContextWindow(200_000)).toBe("200K");
  });

  it("renders millions with an M suffix", () => {
    expect(formatContextWindow(1_000_000)).toBe("1M");
    expect(formatContextWindow(1_500_000)).toBe("1.5M");
  });

  it("truncates millions so a limit never reads larger than it is", () => {
    expect(formatContextWindow(1_050_000)).toBe("1M");
    expect(formatContextWindow(1_990_000)).toBe("1.9M");
  });

  it("renders small counts verbatim", () => {
    expect(formatContextWindow(512)).toBe("512");
  });
});

describe("ProviderIcon", () => {
  it("renders a brand mark for a known provider", () => {
    const markup = renderToStaticMarkup(<ProviderIcon provider="anthropic" />);
    expect(markup).toContain("<svg");
    expect(markup).toContain("<path");
  });

  it("falls back to a generic glyph for unknown providers", () => {
    const known = renderToStaticMarkup(<ProviderIcon provider="openai" />);
    const unknown = renderToStaticMarkup(
      <ProviderIcon provider="openai_compatible" />,
    );
    expect(known).not.toBe(unknown);
  });
});

describe("ModelCapabilities", () => {
  it("shows the context window and both capability markers when supported", () => {
    const markup = renderToStaticMarkup(<ModelCapabilities model={MODELS[0]} />);
    expect(markup).toContain("200K");
    expect(markup).toContain("Accepts image input");
    expect(markup).toContain("Adjustable reasoning effort");
  });

  it("hides the reasoning marker when the model does not support it", () => {
    const markup = renderToStaticMarkup(<ModelCapabilities model={MODELS[1]} />);
    expect(markup).toContain("128K");
    expect(markup).toContain("Accepts image input");
    expect(markup).not.toContain("Adjustable reasoning effort");
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
      reasoningEffortOptions([
        "low",
        "ultra" as never,
        "max",
      ]).map((option) => option.value),
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
    expect(markup).toContain('aria-label="Reasoning effort: Extra High"');
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
      <ReasoningEffortMenu
        levels={LEVELS}
        value={null}
        disabled
        onChange={() => {}}
      />,
    );
    expect(markup).toContain("disabled");
  });
});
