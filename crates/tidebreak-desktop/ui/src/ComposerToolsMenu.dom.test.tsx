// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";

import { ComposerToolsMenu } from "./ComposerToolsMenu";
import { useFirstTaskGuide } from "./FirstTaskWalkthrough";

vi.mock("sonner", () => ({ toast: { warning: vi.fn() } }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  useFirstTaskGuide.getState().setSurface(null);
});

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
    "NetworkOffline",
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

it("warns that changing reasoning effort may prevent cache reuse", async () => {
  const onChange = vi.fn();
  render(
    <ComposerToolsMenu
      disabled={false}
      reasoning={{ levels: LEVELS, value: "high", onChange }}
    />,
  );

  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Tools" }));
  const reasoning = screen.getByRole("menuitem", { name: /Reasoning/ });
  reasoning.focus();
  await user.keyboard("{ArrowRight}{ArrowDown}{Enter}");

  expect(onChange).toHaveBeenCalledWith("low");
  expect(toast.warning).toHaveBeenCalledWith("Prompt cache may not be reused", {
    description:
      "This change may prevent prompt cache reuse, increasing cost and latency on the next turn.",
  });
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

it("offers the plugin library as one row under the turn's setup actions", async () => {
  const onOpen = vi.fn();
  render(
    <ComposerToolsMenu
      disabled={false}
      attachFiles={{ attaching: false, onAttach: vi.fn() }}
      plugins={{ onOpen }}
    />,
  );

  await open();
  const rows = screen.getAllByRole("menuitem");
  expect(rows.map((row) => row.textContent)).toEqual([
    "Attach files",
    "Plugins",
  ]);

  await userEvent.setup().click(rows[1]);
  await waitFor(() => expect(onOpen).toHaveBeenCalledOnce());
});

it("keeps the plugins row off a surface with no library to reach", async () => {
  render(
    <ComposerToolsMenu
      disabled={false}
      attachFiles={{ attaching: false, onAttach: vi.fn() }}
    />,
  );

  await open();
  expect(screen.queryByText("Plugins")).not.toBeInTheDocument();
});

it("opens when the first-task walkthrough is on the tools step", () => {
  useFirstTaskGuide.getState().setSurface("tools");
  render(
    <ComposerToolsMenu
      disabled={false}
      attachFiles={{ attaching: false, onAttach: vi.fn() }}
      network={{ value: { mode: "off" }, onChange: vi.fn() }}
    />,
  );

  expect(screen.getByRole("menuitem", { name: "Attach files" })).toHaveAttribute(
    "data-first-task-target",
    "attach-files",
  );
  expect(screen.getByRole("menuitem", { name: /Network/ })).toHaveAttribute(
    "data-first-task-target",
    "network",
  );
});
