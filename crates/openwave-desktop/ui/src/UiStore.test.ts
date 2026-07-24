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

  it("carries a target into the surface that accepts it", () => {
    const store = createUiStore();

    store.getState().showDocuments({ documentId: "doc-1", citationId: "c-4" });
    expect(store.getState().surface).toEqual({
      kind: "documents",
      itemId: "doc-1",
      anchor: "c-4",
    });

    store.getState().showDeliverables({ filename: "summary.md" });
    expect(store.getState().surface).toEqual({
      kind: "deliverables",
      itemId: "summary.md",
    });
  });

  it("drops a citation that has no source to anchor to", () => {
    const store = createUiStore();
    store.getState().showDocuments({ citationId: "c-4" });
    expect(store.getState().surface).toEqual({ kind: "documents" });
  });

  it("validates a descriptor opened directly", () => {
    const store = createUiStore();

    store.getState().openSurface({ kind: "deliverables", itemId: "notes.md" });
    expect(store.getState().surface).toEqual({
      kind: "deliverables",
      itemId: "notes.md",
    });

    store.getState().openSurface({ kind: "nope" } as never);
    expect(store.getState().surface).toEqual({ kind: "chat" });
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
