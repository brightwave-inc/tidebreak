// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { TurnReviewCard } from "./TurnReviewCard";

afterEach(() => {
  cleanup();
});

describe("TurnReviewCard", () => {
  it("shows diffstat, duration, and opens the turn diff", async () => {
    const onOpenDiff = vi.fn();
    render(
      <TurnReviewCard
        status="completed"
        durationMs={1500}
        usage={{
          input_tokens: 12,
          output_tokens: 3,
          cache_read_input_tokens: 0,
          cache_creation_input_tokens: 0,
        }}
        error={null}
        diffstat={{ files: 2, insertions: 10, deletions: 1, truncated: false }}
        onOpenDiff={onOpenDiff}
      />,
    );
    expect(screen.getByText("Turn completed")).toBeInTheDocument();
    expect(screen.getByText("1.5s")).toBeInTheDocument();
    expect(screen.getByText("2 files +10 −1")).toBeInTheDocument();
    await userEvent.setup().click(screen.getByRole("button", { name: "Review diff" }));
    expect(onOpenDiff).toHaveBeenCalledOnce();
  });
});
