// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeComposer } from "./CodeComposer";
import { PERMISSION_MODE_UNAVAILABLE_REASON } from "./labels";

afterEach(() => {
  cleanup();
});

describe("CodeComposer", () => {
  it("shows Ask and Auto as disabled with the server's unavailable reason", () => {
    render(
      <CodeComposer
        running={false}
        permissionMode="plan"
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "Plan" })).toBeEnabled();
    const ask = screen.getByRole("button", { name: /Ask/ });
    const auto = screen.getByRole("button", { name: /Auto/ });
    expect(ask).toBeDisabled();
    expect(auto).toBeDisabled();
    expect(ask).toHaveAttribute(
      "title",
      `Ask: ${PERMISSION_MODE_UNAVAILABLE_REASON}`,
    );
    expect(auto).toHaveAttribute(
      "title",
      `Auto: ${PERMISSION_MODE_UNAVAILABLE_REASON}`,
    );
    expect(
      screen.getAllByText(PERMISSION_MODE_UNAVAILABLE_REASON),
    ).toHaveLength(2);
  });
});
