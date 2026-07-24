import { describe, expect, it } from "vitest";
import { createUiStore } from "./UiStore";

describe("UiStore", () => {
  it("showChat closes panels by default but preserves them when asked", () => {
    const store = createUiStore();
    store.getState().toggleFoldersPanel();
    expect(store.getState().settingsPanel).toBe("folders");

    // Back-navigation and new-chat historically left the panel alone.
    store.getState().showChat({ keepPanels: true });
    expect(store.getState().primaryView).toBe("chat");
    expect(store.getState().settingsPanel).toBe("folders");

    // Selecting a chat clears it.
    store.getState().showChat();
    expect(store.getState().settingsPanel).toBeNull();
  });

  it("documents, outputs, and settings views always close the folders panel", () => {
    const store = createUiStore();
    store.getState().toggleFoldersPanel();
    store.getState().showDocuments();
    expect(store.getState()).toMatchObject({
      primaryView: "documents",
      settingsPanel: null,
    });

    store.getState().toggleFoldersPanel();
    store.getState().showDeliverables();
    expect(store.getState()).toMatchObject({
      primaryView: "deliverables",
      settingsPanel: null,
    });

    store.getState().toggleFoldersPanel();
    store.getState().showSettings();
    expect(store.getState()).toMatchObject({
      primaryView: "settings",
      settingsPanel: null,
    });
  });

  it("toggleFoldersPanel flips the panel and can force the chat view", () => {
    const store = createUiStore();
    store.getState().showSettings();
    store.getState().toggleFoldersPanel({ showChat: true });
    expect(store.getState()).toMatchObject({
      primaryView: "chat",
      settingsPanel: "folders",
    });
    store.getState().toggleFoldersPanel();
    expect(store.getState().settingsPanel).toBeNull();
  });
});
