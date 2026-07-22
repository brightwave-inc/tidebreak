import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ModelMenu } from "./ModelMenu";
import type { ModelInfo } from "./api";

const MODELS: ModelInfo[] = [
  { id: "claude-sonnet-4", provider: "anthropic" },
  { id: "gpt-4o", provider: "openai" },
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

  it("labels the trigger with a selected known model id", () => {
    const markup = triggerMarkup("gpt-4o");
    expect(markup).toContain('aria-label="Model: gpt-4o"');
    expect(markup).toContain(">gpt-4o<");
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
