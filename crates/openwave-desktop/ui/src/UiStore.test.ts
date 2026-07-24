import { describe, expect, it } from "vitest";
import { createUiStore } from "./UiStore";

describe("UiStore", () => {
  it("each navigation action selects its surface", () => {
    const store = createUiStore();
    expect(store.getState().surface).toEqual({ kind: "chat" });

    store.getState().showDocuments();
    expect(store.getState().surface).toEqual({ kind: "documents" });

    store.getState().showDeliverables();
    expect(store.getState().surface).toEqual({ kind: "deliverables" });

    store.getState().showFolders();
    expect(store.getState().surface).toEqual({ kind: "folders" });

    store.getState().showSettings();
    expect(store.getState().surface).toEqual({ kind: "settings" });

    store.getState().showChat();
    expect(store.getState().surface).toEqual({ kind: "chat" });
  });

  it("carries a target into the one surface whose view resolves it", () => {
    const store = createUiStore();

    store.getState().showDeliverables({ filename: "summary.md" });
    expect(store.getState().surface).toEqual({
      kind: "deliverables",
      itemId: "summary.md",
    });

    // Sources has no target: the view takes only a conversation, so carrying
    // one would read as a feature and behave as dead weight.
    store.getState().showDocuments();
    expect(store.getState().surface).toEqual({ kind: "documents" });
  });

  it("does not republish the surface already open", () => {
    const store = createUiStore();
    store.getState().showDeliverables({ filename: "summary.md" });
    const opened = store.getState().surface;

    store.getState().showDeliverables({ filename: "summary.md" });
    // Same identity, not merely equal: a fresh descriptor would re-render every
    // subscriber for a navigation that did not happen.
    expect(store.getState().surface).toBe(opened);
  });
});

describe("sidebar collapse", () => {
  it("toggles and reports the collapsed state", () => {
    const store = createUiStore();
    expect(store.getState().sidebarCollapsed).toBe(false);
    store.getState().toggleSidebar();
    expect(store.getState().sidebarCollapsed).toBe(true);
    store.getState().toggleSidebar();
    expect(store.getState().sidebarCollapsed).toBe(false);
  });
});
