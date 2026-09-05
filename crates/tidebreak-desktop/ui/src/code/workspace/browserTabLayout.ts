import {
  EMPTY_LAYOUT,
  type LayoutState,
  type PanelContent,
} from "@/panel/panelTypes";
import {
  layoutFromSearch,
  panelSearchFrom,
  searchFromLayout,
} from "@/panel/panelUrl";

export const BROWSER_TABS_STORAGE_PREFIX = "tidebreak.code-browser-tabs.v1.";

/** Tab membership is a renderer preference. Native recovery owns URLs and authority. */
export function browserTabLayout(layout: LayoutState): LayoutState {
  const browsers = (tabs: PanelContent[]) =>
    tabs.filter((tab) => tab.type === "browser");
  const tabs = browsers(layout.tabs);
  const splitTabs = browsers(layout.editorSplit?.tabs ?? []);
  if (tabs.length === 0 && splitTabs.length === 0) return EMPTY_LAYOUT;
  const active = layout.tabs[layout.activeIndex];
  const splitActive = layout.editorSplit?.tabs[layout.editorSplit.activeIndex];
  return {
    tabs,
    activeIndex: Math.max(
      0,
      tabs.findIndex(
        (tab) =>
          active?.type === "browser" && tab.browserId === active.browserId,
      ),
    ),
    fullscreen: layout.fullscreen,
    conversationFocused:
      layout.conversationFocused ||
      (active?.type !== "browser" && !layout.editorSplit?.focused) ||
      undefined,
    editorSplit:
      splitTabs.length > 0
        ? {
            tabs: splitTabs,
            activeIndex: Math.max(
              0,
              splitTabs.findIndex(
                (tab) =>
                  splitActive?.type === "browser" &&
                  tab.browserId === splitActive.browserId,
              ),
            ),
            focused: layout.editorSplit?.focused,
          }
        : undefined,
  };
}

export function readBrowserTabLayout(
  workspaceId: string,
  storage?: Pick<Storage, "getItem">,
): LayoutState {
  try {
    const raw = (storage ?? window.localStorage).getItem(
      BROWSER_TABS_STORAGE_PREFIX + workspaceId,
    );
    if (!raw || raw.length > 65_536) return EMPTY_LAYOUT;
    const value: unknown = JSON.parse(raw);
    if (!value || typeof value !== "object" || Array.isArray(value))
      return EMPTY_LAYOUT;
    return browserTabLayout(
      layoutFromSearch(panelSearchFrom(value as Record<string, unknown>)),
    );
  } catch {
    return EMPTY_LAYOUT;
  }
}

export function writeBrowserTabLayout(
  workspaceId: string,
  layout: LayoutState,
  storage?: Pick<Storage, "setItem" | "removeItem">,
): void {
  try {
    const destination = storage ?? window.localStorage;
    const key = BROWSER_TABS_STORAGE_PREFIX + workspaceId;
    const saved = browserTabLayout(layout);
    if (saved.tabs.length === 0 && !saved.editorSplit) {
      destination.removeItem(key);
    } else {
      destination.setItem(key, JSON.stringify(searchFromLayout(saved)));
    }
  } catch {
    // Tab operations remain available when preference storage is unavailable.
  }
}
