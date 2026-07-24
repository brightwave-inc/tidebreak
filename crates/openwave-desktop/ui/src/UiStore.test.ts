import { describe, expect, it } from "vitest";
import { createUiStore } from "./UiStore";

describe("UiStore", () => {
  it("each navigation action selects its primary view", () => {
    const store = createUiStore();
    expect(store.getState().primaryView).toBe("chat");

    store.getState().showDocuments();
    expect(store.getState().primaryView).toBe("documents");

    store.getState().showDeliverables();
    expect(store.getState().primaryView).toBe("deliverables");

    store.getState().showFolders();
    expect(store.getState().primaryView).toBe("folders");

    store.getState().showSettings();
    expect(store.getState().primaryView).toBe("settings");

    store.getState().showChat();
    expect(store.getState().primaryView).toBe("chat");
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
