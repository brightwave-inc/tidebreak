// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { rankPaletteRows, type PaletteRow } from "./CommandPalette";
import { CommandPaletteList } from "./CommandPaletteList";

afterEach(cleanup);

function row(
  id: string,
  section: PaletteRow["section"],
  label: string,
  extra: Partial<PaletteRow> = {},
): PaletteRow {
  return { id, section, label, onSelect: vi.fn(), ...extra };
}

const ROWS: PaletteRow[] = [
  row("suggested:push", "suggested", "Push", { tone: "ready" }),
  row("workspace:a", "workspaces", "Pluggable memory system"),
  row("action:archive", "actions", "Archive", {
    shortcut: "code-archive-workspace",
  }),
  row("settings:models", "settings", "Models"),
];

function mount(props: Partial<Parameters<typeof CommandPaletteList>[0]> = {}) {
  const onQueryChange = vi.fn();
  const onSelect = vi.fn();
  render(
    <CommandPaletteList
      groups={rankPaletteRows(ROWS, "")}
      query=""
      onQueryChange={onQueryChange}
      onSelect={onSelect}
      command
      {...props}
    />,
  );
  return { onQueryChange, onSelect };
}

describe("command palette list", () => {
  it("draws each section under its own heading, suggestion first", () => {
    mount();
    for (const heading of ["Suggested", "Workspaces", "Actions", "Settings"]) {
      expect(screen.getByText(heading)).toBeInTheDocument();
    }
    const options = screen.getAllByRole("option").map((node) => node.ariaLabel);
    expect(options[0]).toBe("Push");
  });

  it("teaches the chord a row already has", () => {
    // Drawn from the shortcut table rather than written here, so a row can
    // never advertise a key that does something else.
    mount();
    const archive = screen.getByRole("option", { name: "Archive" });
    expect(archive).toHaveTextContent("⌘");
    expect(archive).toHaveTextContent("⇧");
    expect(archive).toHaveTextContent("A");
  });

  it("hands the whole row back on pick, not just its id", () => {
    const { onSelect } = mount();
    screen.getByRole("option", { name: "Models" }).click();
    expect(onSelect).toHaveBeenCalledWith(
      expect.objectContaining({ id: "settings:models" }),
    );
  });

  it("names the scope a prefix put the reader in", () => {
    mount({
      query: ">",
      scopeLabel: "commands",
      groups: rankPaletteRows(ROWS, ">"),
    });
    expect(screen.getByText("commands")).toBeInTheDocument();
    // Inside a scope the footer explains the way out rather than listing the
    // other prefixes.
    expect(screen.getByText("back")).toBeInTheDocument();
  });

  it("drops the scope on backspace at an empty query", () => {
    const { onQueryChange } = mount({
      query: "",
      scopeLabel: "commands",
      groups: rankPaletteRows(ROWS, ">"),
    });
    screen.getByRole("combobox").focus();
    return userEvent.keyboard("{Backspace}").then(() => {
      expect(onQueryChange).toHaveBeenCalledWith("");
    });
  });

  it("says what did not match, in the reader's own words", () => {
    mount({
      query: "zzzz",
      groups: rankPaletteRows(ROWS, "zzzz"),
      emptyLabel: "Nothing matches “zzzz”.",
    });
    expect(screen.getByText("Nothing matches “zzzz”.")).toBeInTheDocument();
  });
});
