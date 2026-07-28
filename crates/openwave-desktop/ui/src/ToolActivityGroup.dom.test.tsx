// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ToolActivityGroup } from "./ToolActivityGroup";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("ToolActivityGroup rail animation", () => {
  it("types the phase label once, then applies later changes instantly", async () => {
    vi.useFakeTimers();
    const { rerender } = render(
      <ToolActivityGroup
        groupIndex={0}
        activities={[{ id: "search", name: "web_search", status: "running" }]}
      />,
    );

    // The first live label types in, so it starts shorter than its final text.
    const label = screen.getByLabelText("Searching the web");
    expect(label.textContent).not.toBe("Searching the web");
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1_000);
    });
    expect(label.textContent).toBe("Searching the web");

    // The phase settling changes the wording; that later change is applied
    // instantly rather than re-typing the whole line.
    rerender(
      <ToolActivityGroup
        groupIndex={0}
        activities={[{ id: "search", name: "web_search", status: "completed" }]}
      />,
    );
    expect(screen.getByLabelText("Searched the web").textContent).toBe(
      "Searched the web",
    );
  });

  it("renders expanded row titles at full length without animating them", () => {
    render(
      <ToolActivityGroup
        groupIndex={0}
        activities={[
          { id: "search", name: "web_search", status: "running" },
          { id: "read", name: "read_file", status: "running" },
        ]}
      />,
    );
    fireEvent.click(screen.getByRole("button"));

    // Row titles are plain text — present in full without advancing any timer.
    expect(screen.getByText("Reading a file")).toBeTruthy();
  });
});
