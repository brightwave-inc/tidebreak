// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { ComposerToolsMenu } from "./ComposerToolsMenu";
import type { PluginInfo } from "./api";

afterEach(cleanup);

const LEVELS = ["low", "medium", "high", "xhigh", "max"] as const;

function plugin(overrides: Partial<PluginInfo> = {}): PluginInfo {
  return {
    name: "documents",
    display_name: "Documents",
    description: "Writes Word, Excel, and PowerPoint files.",
    category: "documents",
    origin: "builtin",
    capabilities: [],
    compatibility: { status: "compatible", issues: [] },
    enabled: false,
    skills: [],
    ...overrides,
  };
}

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

it("offers the installed plugins under the turn's setup actions", async () => {
  const onSelect = vi.fn();
  render(
    <ComposerToolsMenu
      disabled={false}
      attachFiles={{ attaching: false, onAttach: vi.fn() }}
      plugins={{ items: [plugin()], onSelect }}
    />,
  );

  await open();
  const rows = screen.getAllByRole("menuitem");
  expect(rows.map((row) => row.textContent)).toEqual([
    "Attach files",
    "DocumentsWrites Word, Excel, and PowerPoint files.",
  ]);

  await userEvent.setup().click(rows[1]);
  expect(onSelect).toHaveBeenCalledWith(expect.objectContaining({ name: "documents" }));
});

it("keeps the plugins section off a catalog that is empty or unread", async () => {
  render(
    <ComposerToolsMenu
      disabled={false}
      attachFiles={{ attaching: false, onAttach: vi.fn() }}
      plugins={{ items: [], onSelect: vi.fn() }}
    />,
  );

  await open();
  expect(screen.queryByText("Plugins")).not.toBeInTheDocument();
});
