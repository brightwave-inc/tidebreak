// @vitest-environment jsdom
import { cleanup, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Chat } from "./api";
import { AppContextProvider, type AppContextValue } from "./AppContext";
import { ChatHeaderTitle } from "./ChatHeaderTitle";
import { setAttachedRemotely } from "./host";
import { renderWithRouter } from "./test/router";

// The rows are rendered natively from the event journal on this computer, so
// the gate is the whole subject of this file.
const isTauri = vi.hoisted(() => vi.fn(() => true));
vi.mock("@tauri-apps/api/core", () => ({ isTauri, invoke: vi.fn() }));

const chat = {
  id: "chat-1",
  title: "Roadmap",
  created_at: "2026-01-01T00:00:00Z",
  updated_at: "2026-01-01T00:00:00Z",
} as unknown as Chat;

async function openMenu() {
  const user = userEvent.setup();
  await renderWithRouter(
    <AppContextProvider
      value={
        {
          startRename: vi.fn(),
          commitRename: vi.fn(),
          cancelRename: vi.fn(),
          deleteChat: vi.fn(),
        } as unknown as AppContextValue
      }
    >
      <ChatHeaderTitle chat={chat} />
    </AppContextProvider>,
    { initialUrl: "/c/chat-1" },
  );
  await user.click(screen.getByRole("button", { name: "Work menu" }));
}

describe("ChatHeaderTitle diagnostics", () => {
  afterEach(() => {
    cleanup();
    setAttachedRemotely(false);
  });

  it("offers the debug bundle for a conversation on this computer", async () => {
    await openMenu();
    expect(
      screen.getByRole("menuitem", { name: "Copy debug info" }),
    ).toBeInTheDocument();
  });

  it("leaves the debug bundle out while the work is on another machine", async () => {
    setAttachedRemotely(true);
    await openMenu();
    // The bundle is built from the journal on this computer, so there is
    // nothing to build for a conversation that lives elsewhere. Hiding beats
    // offering a row that can only refuse.
    expect(
      screen.queryByRole("menuitem", { name: "Copy debug info" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("menuitem", { name: "Save debug bundle…" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("menuitem", { name: "Rename" }),
    ).toBeInTheDocument();
  });
});
