import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ModelMenu } from "./ModelMenu";
import type { ModelInfo } from "./api";

const MODELS: ModelInfo[] = [
  {
    id: "claude-sonnet-4",
    display_name: "Claude Sonnet 4",
    provider: "anthropic",
    context_window: 200_000,
    supports_reasoning_effort: true,
    multimodal: true,
  },
  {
    id: "gpt-4o",
    display_name: "GPT-4o",
    provider: "openai",
    context_window: 128_000,
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
