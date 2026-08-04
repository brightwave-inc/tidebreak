// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { ComposerToolsMenu } from "./ComposerToolsMenu";

afterEach(cleanup);

const LEVELS = ["low", "medium", "high", "xhigh", "max"] as const;

function open() {
  return userEvent.setup().click(screen.getByRole("button", { name: "Tools" }));
}

it("gathers the turn's setup actions behind one button", async () => {
  const attachFiles = vi.fn();
  const attachFolder = vi.fn();
  render(
    <ComposerToolsMenu
      disabled={false}
      attachFiles={{ attaching: false, onAttach: attachFiles }}
      attachFolder={{ working: false, onAttach: attachFolder }}
      reasoning={{ levels: LEVELS, value: "high", onChange: vi.fn() }}
      network={{ value: { mode: "off" }, onChange: vi.fn() }}
    />,
  );

  await open();
  // The order the menu presents: attach first, then the settings a turn runs
  // under. Both settings name their current value on the row.
  expect(
    screen.getAllByRole("menuitem").map((item) => item.textContent),
  ).toEqual([
    "Attach files",
    "Attach folder",
    "ReasoningHigh",
    "NetworkNetwork off",
  ]);

  await userEvent.setup().click(
    screen.getByRole("menuitem", { name: "Attach files" }),
  );
  expect(attachFiles).toHaveBeenCalledOnce();
});

it("still names an effort level the current model no longer accepts", async () => {
  render(
    <ComposerToolsMenu
      disabled={false}
      reasoning={{
        levels: ["none", "low", "medium", "high", "xhigh"],
        value: "max",
        onChange: vi.fn(),
      }}
    />,
  );

  await open();
  expect(screen.getByRole("menuitem", { name: /Reasoning/ })).toHaveTextContent(
    "Max",
  );
});

it("opens the network policy in a dialog the menu does not clip", async () => {
  const user = userEvent.setup();
  render(
    <ComposerToolsMenu
      disabled={false}
      network={{ value: { mode: "open" }, onChange: vi.fn() }}
    />,
  );

  await user.click(screen.getByRole("button", { name: "Tools" }));
  await user.click(screen.getByRole("menuitem", { name: /Network/ }));
  await waitFor(() =>
    expect(
      screen.getByRole("dialog", { name: "Code execution network" }),
    ).toBeInTheDocument(),
  );
});

it("renders nothing when the surface offers none of its actions", () => {
  const { container } = render(<ComposerToolsMenu disabled={false} />);
  expect(container).toBeEmptyDOMElement();
});
