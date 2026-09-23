// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderToStaticMarkup } from "react-dom/server";

import {
  ContextUsageIndicator,
  type ContextUsageReading,
} from "./ContextUsageIndicator";

afterEach(cleanup);

/**
 * A real code-mode turn: six model calls, so the four spend counts total far
 * more than the window while the prompt still resident is a quarter of it.
 */
const SIX_CALL_TURN: ContextUsageReading = {
  contextTokens: 44_172,
  spend: {
    input: 1_244,
    output: 12_902,
    cacheRead: 236_180,
    cacheWrite: 8_412,
  },
  contextWindow: 200_000,
  modelName: "Sonnet 5",
};

describe("the ring reads context, not spend", () => {
  it("fills from the resident prompt when the spend runs past the window", () => {
    // Measured on a real lane: 258,738 summed against a 44,172 prompt. The
    // sum clamps to 100% and colours the ring critical; the prompt is 22%.
    const markup = renderToStaticMarkup(
      <ContextUsageIndicator {...SIX_CALL_TURN} />,
    );

    expect(markup).toContain('aria-label="Context: 22% of 200k tokens used"');
    expect(markup).not.toContain("100%");
    expect(markup).not.toContain("text-critical");
  });

  it("escalates on the resident prompt alone", () => {
    const markup = renderToStaticMarkup(
      <ContextUsageIndicator {...SIX_CALL_TURN} contextTokens={185_000} />,
    );

    expect(markup).toContain('aria-label="Context: 93% of 200k tokens used"');
    expect(markup).toContain("text-critical");
  });
});

describe("a filling window reads as a meter, not a spinner", () => {
  it("prints its percent once the window reaches the warning level", () => {
    render(
      <ContextUsageIndicator {...SIX_CALL_TURN} contextTokens={160_000} />,
    );

    const ring = screen.getByRole("button", {
      name: "Context: 80% of 200k tokens used",
    });
    // The ring is a mark and takes the full-strength tone; the percent beside
    // it is text and takes the readable rung.
    expect(ring).toHaveClass("text-warning");
    expect(ring).not.toHaveClass("text-warning-foreground");
    expect(within(ring).getByText("80%")).toHaveClass(
      "text-warning-foreground",
    );
  });

  it("prints its percent in the critical tone near the limit", () => {
    render(
      <ContextUsageIndicator {...SIX_CALL_TURN} contextTokens={185_000} />,
    );

    const ring = screen.getByRole("button", {
      name: "Context: 93% of 200k tokens used",
    });
    expect(ring).toHaveClass("text-critical");
    expect(within(ring).getByText("93%")).toHaveClass(
      "text-critical-foreground",
    );
  });

  it("stays a bare ring while the window has room", () => {
    render(<ContextUsageIndicator {...SIX_CALL_TURN} />);

    const ring = screen.getByRole("button", {
      name: "Context: 22% of 200k tokens used",
    });
    expect(ring).toHaveClass("text-muted-foreground");
    expect(within(ring).queryByText("22%")).toBeNull();
  });
});

describe("readings the engine did not publish", () => {
  it("shows no percent when there is no context figure", () => {
    const markup = renderToStaticMarkup(
      <ContextUsageIndicator {...SIX_CALL_TURN} contextTokens={null} />,
    );

    expect(markup).toContain(
      'aria-label="Context: no reading from this engine"',
    );
    expect(markup).not.toContain("% of");
    expect(markup).not.toContain("text-critical");
  });

  it("treats a zero from the engine as no reading, not an empty window", () => {
    const markup = renderToStaticMarkup(
      <ContextUsageIndicator {...SIX_CALL_TURN} contextTokens={0} />,
    );

    expect(markup).toContain(
      'aria-label="Context: no reading from this engine"',
    );
    expect(markup).not.toContain("0%");
  });

  it("keeps the token count when only the window is missing", () => {
    const markup = renderToStaticMarkup(
      <ContextUsageIndicator {...SIX_CALL_TURN} contextWindow={undefined} />,
    );

    expect(markup).toContain('aria-label="Context: 44,172 tokens used"');
    expect(markup).not.toContain("% of");
  });
});

describe("the hover", () => {
  it("names the four counts as spend and states the context separately", async () => {
    const user = userEvent.setup();
    render(<ContextUsageIndicator {...SIX_CALL_TURN} />);

    await user.hover(screen.getByRole("button"));

    const tooltip = await screen.findByRole("tooltip");
    // The four counts are labelled as spend, so nobody reads 236,180 cached
    // tokens as a window that overflowed.
    expect(tooltip).toHaveTextContent("Turn spend");
    expect(tooltip).toHaveTextContent("236,180");
    // And the occupancy reading is stated in full beside them.
    expect(tooltip).toHaveTextContent("44,172 / 200,000");
  });
});
