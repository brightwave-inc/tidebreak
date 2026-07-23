import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ModelCapabilities,
  ModelMenu,
  ProviderIcon,
  ReasoningEffortMenu,
  formatContextWindow,
} from "./ModelMenu";
import type { ModelInfo } from "./api";

const MODELS: ModelInfo[] = [
  {
    id: "claude-sonnet-4",
    display_name: "Claude Sonnet 4",
    provider: "anthropic",
    context_window: 200_000,
    max_output_tokens: 64_000,
    input_modalities: ["text", "image"],
    supports_reasoning: true,
    supports_reasoning_effort: true,
    multimodal: true,
  },
  {
    id: "gpt-4o",
    display_name: "GPT-4o",
    provider: "openai",
    context_window: 128_000,
    max_output_tokens: 16_384,
    input_modalities: ["text", "image"],
    supports_reasoning: false,
    supports_reasoning_effort: false,
    multimodal: true,
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
    const markup = triggerMarkup("gpt-4o");
    expect(markup).toContain('aria-label="Model: GPT-4o"');
    expect(markup).toContain(">GPT-4o<");
  });

  it("labels the trigger with an unknown (custom) override verbatim", () => {
    const markup = triggerMarkup("local-model:latest");
    expect(markup).toContain('aria-label="Model: local-model:latest"');
    expect(markup).toContain(">local-model:latest<");
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

describe("ReasoningEffortMenu", () => {
  it("labels the trigger 'Default' when no effort is set", () => {
    const markup = renderToStaticMarkup(
      <ReasoningEffortMenu value={null} onChange={() => {}} />,
    );
    expect(markup).toContain('aria-label="Reasoning effort: Default"');
    expect(markup).toContain(">Default<");
  });

  it("labels the trigger with the selected effort level", () => {
    const markup = renderToStaticMarkup(
      <ReasoningEffortMenu value="high" onChange={() => {}} />,
    );
    expect(markup).toContain('aria-label="Reasoning effort: High"');
    expect(markup).toContain(">High<");
  });

  it("disables the trigger when asked", () => {
    const markup = renderToStaticMarkup(
      <ReasoningEffortMenu value={null} disabled onChange={() => {}} />,
    );
    expect(markup).toContain("disabled");
  });
});
