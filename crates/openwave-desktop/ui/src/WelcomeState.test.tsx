import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { WelcomeState } from "./WelcomeState";

describe("WelcomeState", () => {
  it("renders the greeting and starter prompts when a handler is provided", () => {
    const markup = renderToStaticMarkup(
      <WelcomeState onSelectPrompt={vi.fn()} />,
    );

    expect(markup).toContain("How can I help?");
    expect(markup).toContain("What can you help me with?");
    expect(markup).toContain("Draft a plan");
    expect(markup).toContain('aria-label="Start a conversation"');
  });

  it("omits the starter prompts when no handler is provided", () => {
    const markup = renderToStaticMarkup(<WelcomeState />);

    expect(markup).toContain("How can I help?");
    expect(markup).not.toContain("welcome-prompt");
  });
});
