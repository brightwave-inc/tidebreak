// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ToolActivityGroup } from "./ToolActivityGroup";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("ToolActivityGroup live copy", () => {
  it("types a changed row title while another tool keeps the phase live", async () => {
    vi.useFakeTimers();
    const { rerender } = render(
      <ToolActivityGroup
        groupIndex={0}
        activities={[
          { id: "search", name: "web_search", status: "running" },
          { id: "read", name: "read_file", status: "running" },
        ]}
      />,
    );

    fireEvent.click(screen.getByRole("button"));
    expect(screen.getByLabelText("Searching the web").textContent).toBe(
      "Searching the web",
    );

    rerender(
      <ToolActivityGroup
        groupIndex={0}
        activities={[
          { id: "search", name: "web_search", status: "completed" },
          { id: "read", name: "read_file", status: "running" },
        ]}
      />,
    );

    const completedSearch = screen.getByLabelText("Searched the web");
    expect(completedSearch.textContent).not.toBe("Searched the web");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000);
    });
    expect(completedSearch.textContent).toBe("Searched the web");
  });
});
