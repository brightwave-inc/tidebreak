import { describe, expect, it, vi } from "vitest";

import type { Chat, Project } from "./api";
import { appPaletteRows, currentChatPaletteRows } from "./appPaletteRows";
import { rankPaletteRows } from "./CommandPalette";

function chat(overrides: Partial<Chat> = {}): Chat {
  return {
    id: "chat-1",
    title: "Harbour pricing",
    project_id: null,
    created_at: "2026-09-24T12:00:00Z",
    ...overrides,
  } as Chat;
}

const projects = [
  { id: "p-marketing", title: "Marketing" },
  { id: "p-ops", title: "Ops" },
] as Project[];

function chatRows(overrides: Partial<Chat> = {}) {
  const actions = {
    onRename: vi.fn(),
    onDelete: vi.fn(),
    onMove: vi.fn(),
    onExport: vi.fn(),
    onFind: vi.fn(),
  };
  const rows = currentChatPaletteRows({
    chat: chat(overrides),
    projects,
    ...actions,
  });
  const pick = (label: string) => {
    const row = rows.find((candidate) => candidate.label === label);
    if (!row) throw new Error(`no row "${label}"`);
    row.onSelect();
    return row;
  };
  return { rows, actions, pick };
}

describe("palette rows for the chat on screen", () => {
  it("renames, exports, finds, and deletes through the app's own actions", () => {
    const { actions, pick } = chatRows();
    pick("Rename this work");
    pick("Export this work");
    pick("Find in this conversation");
    pick("Delete this work");
    expect(actions.onRename).toHaveBeenCalledOnce();
    expect(actions.onExport).toHaveBeenCalledOnce();
    expect(actions.onFind).toHaveBeenCalledOnce();
    expect(actions.onDelete).toHaveBeenCalledOnce();
  });

  it("offers every other project to move to, and the way out of the current one", () => {
    const loose = chatRows();
    expect(loose.rows.map((row) => row.label)).toEqual(
      expect.arrayContaining(["Move to Marketing", "Move to Ops"]),
    );
    expect(loose.rows.some((row) => row.label === "Remove from project")).toBe(
      false,
    );
    loose.pick("Move to Ops");
    expect(loose.actions.onMove).toHaveBeenCalledWith("p-ops");

    const filed = chatRows({ project_id: "p-marketing" });
    expect(filed.rows.some((row) => row.label === "Move to Marketing")).toBe(
      false,
    );
    filed.pick("Remove from project");
    expect(filed.actions.onMove).toHaveBeenCalledWith(null);
  });

  it("keeps these rows out of the recents memory", () => {
    const { rows } = chatRows();
    expect(rows.every((row) => row.transient)).toBe(true);
  });

  it("is found by what the reader types", () => {
    const { rows } = chatRows();
    const groups = rankPaletteRows(rows, "export");
    expect(groups[0]?.rows[0]?.label).toBe("Export this work");
  });
});

describe("palette rows for the app", () => {
  function appRows(options: { updates?: boolean } = {}) {
    const actions = {
      onTheme: vi.fn(),
      onZoomIn: vi.fn(),
      onZoomOut: vi.fn(),
      onZoomReset: vi.fn(),
      onNotifications: vi.fn(),
      onShortcuts: vi.fn(),
      onCheckForUpdates: options.updates === false ? undefined : vi.fn(),
    };
    const rows = appPaletteRows({ theme: "dark", ...actions });
    return { rows, actions };
  }

  it("opens notifications and the shortcuts, and checks for updates", () => {
    const { rows, actions } = appRows();
    const byId = new Map(rows.map((row) => [row.id, row]));
    byId.get("app:notifications")?.onSelect();
    byId.get("app:shortcuts")?.onSelect();
    byId.get("app:check-for-updates")?.onSelect();
    expect(actions.onNotifications).toHaveBeenCalledOnce();
    expect(actions.onShortcuts).toHaveBeenCalledOnce();
    expect(actions.onCheckForUpdates).toHaveBeenCalledOnce();
    expect(byId.get("app:shortcuts")?.shortcut).toBe("show-shortcuts");
  });

  it("leaves out the update check where the app cannot update itself", () => {
    const { rows } = appRows({ updates: false });
    expect(rows.some((row) => row.id === "app:check-for-updates")).toBe(false);
  });

  it("switches the theme and marks the current one", () => {
    const { rows, actions } = appRows();
    const dark = rows.find((row) => row.id === "app:theme:dark");
    expect(dark?.hint).toBe("Current");
    rows.find((row) => row.id === "app:theme:light")?.onSelect();
    expect(actions.onTheme).toHaveBeenCalledWith("light");
  });

  it("zooms with the same chords the keyboard uses", () => {
    const { rows, actions } = appRows();
    for (const id of ["app:zoom-in", "app:zoom-out", "app:zoom-reset"]) {
      rows.find((row) => row.id === id)?.onSelect();
    }
    expect(actions.onZoomIn).toHaveBeenCalledOnce();
    expect(actions.onZoomOut).toHaveBeenCalledOnce();
    expect(actions.onZoomReset).toHaveBeenCalledOnce();
    expect(rows.find((row) => row.id === "app:zoom-in")?.shortcut).toBe(
      "zoom-in",
    );
  });
});
