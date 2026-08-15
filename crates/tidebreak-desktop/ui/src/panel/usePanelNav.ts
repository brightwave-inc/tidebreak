import { useMemo } from "react";
import { useNavigate, useParams, useSearch } from "@tanstack/react-router";

import { EMPTY_LAYOUT, panelKey, type LayoutState, type PanelContent } from "./panelTypes";
import { layoutFromSearch, searchFromLayout, type PanelSearch } from "./panelUrl";

/** The layout the URL currently describes. */
export function useLayoutState(): LayoutState {
  const search = useSearch({ strict: false }) as PanelSearch;
  return useMemo(
    () => layoutFromSearch(search),
    [search.tabs, search.active, search.fullscreen, search.left, search.right],
  );
}

/**
 * Opening, closing, and expanding panels — all of it by navigation, because
 * the URL is where the layout lives. The conversation is never one of them: it
 * holds its own region and cannot be displaced, so every helper here only
 * rearranges the tabs beside it.
 */
export function usePanelNav() {
  const navigate = useNavigate();
  const layout = useLayoutState();
  const { chatId, workspaceId } = useParams({ strict: false }) as {
    chatId?: string;
    workspaceId?: string;
  };

  function go(next: LayoutState) {
    // Panels live beside a conversation when there is one; outside any
    // conversation the home route hosts them (the Apps library), and its bare
    // URL is the no-tabs collapse the same way a chat's is. Code workspaces
    // host the same strip beside their transcript.
    if (workspaceId) {
      void navigate({
        to: "/code/w/$workspaceId",
        params: { workspaceId },
        search: searchFromLayout(next),
      });
    } else if (chatId) {
      void navigate({
        to: "/c/$chatId",
        params: { chatId },
        search: searchFromLayout(next),
      });
    } else {
      void navigate({ to: "/", search: searchFromLayout(next) });
    }
  }

  function indexOf(panel: PanelContent): number {
    const key = panelKey(panel);
    return layout.tabs.findIndex((tab) => panelKey(tab) === key);
  }

  return {
    layout,

    /**
     * Show `panel` in the region beside the conversation.
     *
     * A panel already open is brought forward rather than opened twice, and
     * takes on whatever the new request carries — clicking a second citation
     * into an open document moves that tab to the new position instead of
     * stacking another copy of the document beside it.
     */
    openPanel(panel: PanelContent) {
      const existing = indexOf(panel);
      if (existing !== -1) {
        const tabs = layout.tabs.slice();
        tabs[existing] = panel;
        go({ ...layout, tabs, activeIndex: existing });
        return;
      }
      go({
        ...layout,
        tabs: [...layout.tabs, panel],
        activeIndex: layout.tabs.length,
      });
    },

    /**
     * Close one tab — by default the one showing.
     *
     * Focus falls to the tab on its left, which is where the reader was before
     * they opened this one, and the last tab closing leaves the conversation
     * alone at the bare URL.
     */
    closeTab(target?: PanelContent | number) {
      const index =
        typeof target === "number" ? target : target ? indexOf(target) : layout.activeIndex;
      if (index < 0 || index >= layout.tabs.length) return;

      const tabs = layout.tabs.filter((_, at) => at !== index);
      if (tabs.length === 0) {
        go(EMPTY_LAYOUT);
        return;
      }
      let activeIndex = layout.activeIndex;
      if (index < activeIndex) activeIndex -= 1;
      else if (index === activeIndex) activeIndex = index - 1;
      go({
        ...layout,
        tabs,
        activeIndex: Math.min(Math.max(activeIndex, 0), tabs.length - 1),
      });
    },

    /** Bring one of the open tabs forward. */
    focusTab(index: number) {
      if (index < 0 || index >= layout.tabs.length || index === layout.activeIndex) return;
      go({ ...layout, activeIndex: index });
    },

    /** Collapse back to the conversation alone. */
    closeAllPanels() {
      go(EMPTY_LAYOUT);
    },

    /** Hand the window to the region, or give the conversation its share back. */
    toggleFullscreen() {
      if (layout.tabs.length === 0) return;
      go({ ...layout, fullscreen: !layout.fullscreen });
    },
  };
}
