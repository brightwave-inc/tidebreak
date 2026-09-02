// @vitest-environment jsdom
import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { renderWithRouter } from "@/test/router";
import { TurnReviewCard } from "./TurnReviewCard";

afterEach(cleanup);

describe("TurnReviewCard", () => {
  it("links an engine version floor to the coding harnesses settings page", async () => {
    const { router } = await renderWithRouter(
      <TurnReviewCard
        turn={{
          kind: "turn_boundary",
          id: "b-version",
          turnId: "t-version",
          status: "failed",
          durationMs: 4_000,
          usage: null,
          error:
            "API Error: 400 Claude Code 2.1.234 does not support this model; version 2.1.251 or newer is required.",
          diffstat: null,
        }}
      />,
      { initialUrl: "/code" },
    );

    await userEvent.click(
      screen.getByRole("button", { name: "Settings → Coding harnesses" }),
    );
    await waitFor(() =>
      expect(router.state.location.pathname).toBe("/settings/coding-harnesses"),
    );
  });
});
