// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Chat } from "./api";
import { ChatWorkspace } from "./ChatWorkspace";
import { useUiStore } from "./UiStore";

vi.mock("./documents", () => ({
  listLibraryDocuments: vi.fn().mockResolvedValue({
    documents: [],
    truncated: false,
  }),
  importLibraryDocument: vi.fn(),
  searchLibraryDocuments: vi.fn().mockResolvedValue([]),
}));

vi.mock("./deliverables", () => ({
  listDeliverables: vi.fn().mockResolvedValue({
    deliverables: [],
    truncated: false,
  }),
  readDeliverable: vi.fn(),
  exportDeliverable: vi.fn(),
}));

vi.mock("./host", () => ({
  hasNativeHost: () => true,
  listConnectedFolders: vi.fn().mockResolvedValue([]),
  listApprovedFolders: vi.fn().mockResolvedValue([]),
  connectFolder: vi.fn(),
  connectApprovedFolder: vi.fn(),
  disconnectFolder: vi.fn(),
}));

const chat = {
  id: "chat-1",
  title: "Roadmap",
  project_id: null,
} as unknown as Chat;

function renderWorkspace(nativeHost = true) {
  return render(
    <ChatWorkspace
      chat={chat}
      status="chat chat-1 · live"
      nativeHost={nativeHost}
      transcript={<div data-testid="transcript" />}
    />,
  );
}

beforeEach(() => {
  useUiStore.getState().showChat();
});

afterEach(() => {
  cleanup();
  useUiStore.getState().showChat();
});

describe("ChatWorkspace", () => {
  it("shows the chat title and connection status above the surface", () => {
    renderWorkspace();

    expect(
      screen.getByRole("heading", { name: "Roadmap" }),
    ).toBeInTheDocument();
    expect(screen.getByText("chat chat-1 · live")).toBeInTheDocument();
    expect(screen.getByTestId("transcript")).toBeInTheDocument();
  });

  it("keeps one switcher in place when the surface changes", async () => {
    const user = userEvent.setup();
    renderWorkspace();

    expect(screen.getAllByRole("tablist")).toHaveLength(1);

    await user.click(screen.getByRole("tab", { name: "Sources" }));

    expect(screen.getAllByRole("tablist")).toHaveLength(1);
    expect(
      screen.getByRole("heading", { name: "Roadmap" }),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("transcript")).not.toBeInTheDocument();
    expect(
      screen.getByRole("heading", { name: "Sources" }),
    ).toBeInTheDocument();
  });

  it("offers native-host surfaces as unavailable rather than hiding them", () => {
    renderWorkspace(false);

    expect(screen.getByRole("tab", { name: "Chat" })).toBeEnabled();
    for (const label of ["Sources", "Outputs", "Folders"]) {
      const tab = screen.getByRole("tab", { name: label });
      expect(tab).toBeDisabled();
      expect(tab).toHaveAttribute(
        "title",
        "Available in the OpenWave desktop app",
      );
    }
  });

  it("falls back to the transcript when the selected surface needs a host", () => {
    useUiStore.getState().showDocuments();
    renderWorkspace(false);

    expect(screen.getByTestId("transcript")).toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: "Sources" }),
    ).not.toBeInTheDocument();
  });
});
