// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ToolActivityGroup, type ToolActivity } from "./ToolActivityGroup";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
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

  it("shows a replayed active phase immediately", () => {
    render(
      <ToolActivityGroup
        groupIndex={0}
        animate={false}
        activities={[{ id: "write", name: "write_file", status: "running" }]}
      />,
    );

    const label = screen.getByLabelText("Updating a file");
    expect(label.textContent).toBe("Updating a file");
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

describe("an activity that cannot be read", () => {
  it("costs its own row, not the phase's other row or its cards", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    // Reading this activity throws while the group derives its rows — in the
    // group's own body, outside the rail's boundary, so it used to fall to
    // the phase's backstop and take the phase's cards down with it.
    const unreadable = {
      id: "broken",
      status: "completed",
      get name(): never {
        throw new Error("unreadable activity projection");
      },
    } as unknown as ToolActivity;

    render(
      <ToolActivityGroup
        groupIndex={0}
        activities={[
          { id: "read", name: "read_file", status: "completed" },
          unreadable,
        ]}
      >
        <p>a card that must stay reachable</p>
      </ToolActivityGroup>,
    );

    // The cards below the rail are the part a reader may have to act on.
    expect(screen.getByText("a card that must stay reachable")).toBeTruthy();

    fireEvent.click(screen.getByRole("button"));
    // The row beside the unreadable one renders; the unreadable one says so
    // in place rather than vanishing or taking the rail down. (The phase line
    // also reads "Read a file", so the row is found inside the rail itself.)
    const rail = within(screen.getByRole("list"));
    expect(rail.getByText("Read a file")).toBeTruthy();
    expect(
      rail.getAllByText("This step could not be displayed.").length,
    ).toBe(1);
  });
});
