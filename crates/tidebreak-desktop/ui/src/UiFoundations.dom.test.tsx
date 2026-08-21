// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AssistantSources } from "./AssistantSources";
import { FirstTaskWalkthrough } from "./FirstTaskWalkthrough";
import { ThinkingAccordion } from "./ThinkingAccordion";
import { AppearancePanel } from "./settings/AppearancePanel";

afterEach(cleanup);

describe("UI foundations", () => {
  it("hosts the first-task walkthrough on the shared Dialog stack", () => {
    render(
      <>
        <div data-first-task-target="model-choice">choice</div>
        <FirstTaskWalkthrough open onClose={vi.fn()} />
      </>,
    );

    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveAttribute("data-state", "open");
    expect(
      dialog.closest("[data-radix-dialog-content], [role='dialog']"),
    ).toBeTruthy();
    expect(
      screen.getByRole("heading", { name: "Choose a model" }),
    ).toBeInTheDocument();
  });

  it("uses shared radio primitives for the theme picker", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<AppearancePanel mode="system" onChange={onChange} />);

    const radios = screen.getAllByRole("radio");
    expect(radios).toHaveLength(3);
    expect(screen.getByRole("radio", { name: "System" })).toHaveAttribute(
      "data-state",
      "checked",
    );

    await user.click(screen.getByRole("radio", { name: "Dark" }));
    expect(onChange).toHaveBeenCalledWith("dark");
  });

  it("toggles thinking and source disclosures through Collapsible", async () => {
    const user = userEvent.setup();
    render(
      <>
        <ThinkingAccordion
          text="I considered the tradeoffs."
          streaming={false}
        />
        <AssistantSources
          sources={[
            {
              id: "s1",
              ordinal: 1,
              documentId: "doc-1",
              locator: { kind: "page", page: 2 },
            },
          ]}
        />
      </>,
    );

    const thought = screen.getByRole("button", { name: /Thought/i });
    expect(thought).toHaveAttribute("data-state", "closed");
    expect(
      screen.queryByText("I considered the tradeoffs."),
    ).not.toBeInTheDocument();

    await user.click(thought);
    expect(thought).toHaveAttribute("data-state", "open");
    expect(screen.getByText("I considered the tradeoffs.")).toBeInTheDocument();

    const sources = screen.getByRole("button", { name: /1 source/i });
    expect(sources).toHaveAttribute("data-state", "closed");
    fireEvent.click(sources);
    expect(sources).toHaveAttribute("data-state", "open");
    expect(screen.getByText("Page 2")).toBeInTheDocument();
  });
});
