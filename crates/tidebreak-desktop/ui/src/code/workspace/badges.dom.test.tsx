// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { SessionAttentionBadge } from "./badges";

vi.mock("./CodeSessionPane", () => ({
  useRegisteredCodeSession: () => {
    throw new Error("Attention comes from the session digest");
  },
}));

afterEach(cleanup);

it("updates the header when digest attention changes", () => {
  const { container, rerender } = render(
    <SessionAttentionBadge
      attention={{ state: { type: "working" }, source: "lifecycle" }}
    />,
  );
  expect(container).toBeEmptyDOMElement();

  rerender(
    <SessionAttentionBadge
      attention={{
        state: {
          type: "needs_you",
          prompt: "Approve the write",
          source: "structured",
        },
        source: "structured",
      }}
    />,
  );
  expect(screen.getByLabelText("Approve the write")).toHaveAttribute(
    "data-attention",
    "needs_you",
  );

  rerender(
    <SessionAttentionBadge
      attention={{ state: { type: "idle" }, source: "lifecycle" }}
    />,
  );
  expect(container).toBeEmptyDOMElement();
});
