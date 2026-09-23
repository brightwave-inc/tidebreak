// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Chat } from "@/api";

import { RecentChatRow } from "./RecentChatRow";

afterEach(cleanup);

const chat = {
  id: "c1",
  title: "Fix login",
} as Chat;

describe("RecentChatRow", () => {
  it("announces needs attention inside the row button", () => {
    render(
      <RecentChatRow
        chat={chat}
        active={false}
        needsAttention
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
        onDelete={vi.fn()}
      />,
    );

    expect(
      screen.getByRole("button", { name: "Fix login, needs attention" }),
    ).toBeTruthy();
  });
});
