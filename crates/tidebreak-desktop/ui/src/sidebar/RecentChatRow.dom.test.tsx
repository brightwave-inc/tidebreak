// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Chat } from "@/api";

import { RecentChatRow } from "./RecentChatRow";

afterEach(cleanup);

const chat = {
  id: "c1",
  title: "Fix login",
  created_at: "2026-09-20T09:00:00Z",
  last_activity_at: "2026-09-20T10:00:00Z",
  pinned_at: null,
  archived_at: null,
  running: false,
  unread: false,
  turn_count: 2,
} as unknown as Chat;

function renderRow(
  overrides: Partial<Chat> = {},
  props: { active?: boolean; needsAttention?: boolean } = {},
) {
  const handlers = {
    onTogglePin: vi.fn(),
    onArchive: vi.fn(),
    onDelete: vi.fn(),
  };
  render(
    <RecentChatRow
      chat={{ ...chat, ...overrides }}
      active={props.active ?? false}
      needsAttention={props.needsAttention ?? false}
      renaming={false}
      renameDraft=""
      savingTitle={false}
      mutating={false}
      projects={[]}
      onRenameDraftChange={vi.fn()}
      onOpen={vi.fn()}
      onStartRename={vi.fn()}
      onCommitRename={vi.fn()}
      onCancelRename={vi.fn()}
      onMoveToProject={vi.fn()}
      {...handlers}
    />,
  );
  return handlers;
}

describe("RecentChatRow", () => {
  it("announces needs attention inside the row button", () => {
    renderRow({}, { needsAttention: true });
    expect(
      screen.getByRole("button", { name: "Fix login, needs attention" }),
    ).toBeTruthy();
  });

  it("marks a running turn with the live mark", () => {
    renderRow({ running: true });
    expect(
      screen.getByRole("button", { name: "Fix login, working" }),
    ).toBeTruthy();
    expect(document.querySelector('[data-row-state="running"]')).not.toBeNull();
  });

  it("marks a turn that finished while you were elsewhere", () => {
    renderRow({ unread: true });
    expect(
      screen.getByRole("button", { name: "Fix login, new since you looked" }),
    ).toBeTruthy();
  });

  it("does not call the open conversation unread", () => {
    renderRow({ unread: true }, { active: true });
    expect(screen.getByRole("button", { name: "Fix login" })).toBeTruthy();
    expect(document.querySelector("[data-row-state]")).toBeNull();
  });

  it("lets a waiting question outrank a running turn", () => {
    renderRow({ running: true, unread: true }, { needsAttention: true });
    expect(
      screen.getByRole("button", { name: "Fix login, needs attention" }),
    ).toBeTruthy();
  });

  it("offers pin and archive from the row menu", async () => {
    const user = userEvent.setup();
    const handlers = renderRow();
    await user.click(
      screen.getByRole("button", { name: "Actions for Fix login" }),
    );
    await user.click(screen.getByRole("menuitem", { name: "Pin" }));
    expect(handlers.onTogglePin).toHaveBeenCalledOnce();

    await user.click(
      screen.getByRole("button", { name: "Actions for Fix login" }),
    );
    await user.click(screen.getByRole("menuitem", { name: "Archive" }));
    expect(handlers.onArchive).toHaveBeenCalledOnce();
    expect(handlers.onDelete).not.toHaveBeenCalled();
  });

  it("offers unpin for a pinned conversation", async () => {
    const user = userEvent.setup();
    renderRow({ pinned_at: "2026-09-20T10:00:00Z" });
    await user.click(
      screen.getByRole("button", { name: "Actions for Fix login" }),
    );
    expect(screen.getByRole("menuitem", { name: "Unpin" })).toBeTruthy();
  });

  it("leaves pin and archive out for a server older than the list of work", async () => {
    const user = userEvent.setup();
    // The bare conversation such a server sends: no list fields at all.
    renderRow({
      last_activity_at: undefined,
      pinned_at: undefined,
      archived_at: undefined,
      running: undefined,
      unread: undefined,
      turn_count: undefined,
    });
    await user.click(
      screen.getByRole("button", { name: "Actions for Fix login" }),
    );
    expect(screen.getByRole("menuitem", { name: "Rename" })).toBeTruthy();
    expect(screen.queryByRole("menuitem", { name: "Pin" })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: "Archive" })).toBeNull();
    expect(screen.getByRole("menuitem", { name: "Delete" })).toBeTruthy();
  });
});
