import { useMemo } from "react";
import { useNavigate, useParams, useSearch } from "@tanstack/react-router";

import { isContentPanel, type LayoutState, type PanelContent, type PanelPosition } from "./panelTypes";
import { isValidLayout, layoutFromSearch, searchFromLayout, type PanelSearch } from "./panelUrl";

/** The layout the URL currently describes. */
export function useLayoutState(): LayoutState {
  const search = useSearch({ strict: false }) as PanelSearch;
  return useMemo(
    () => layoutFromSearch(search),
    [search.left, search.right, search.fullscreen],
  );
}

/**
 * Opening, closing, and expanding panels — all of it by navigation, because the
 * URL is where the layout lives. Every helper preserves the other slot, so
 * opening sources from a conversation leaves the conversation beside it rather
 * than replacing it.
 */
export function usePanelNav() {
  const navigate = useNavigate();
  const layout = useLayoutState();
  const { chatId } = useParams({ strict: false }) as { chatId?: string };

  function go(next: LayoutState) {
    if (!chatId) return;
    void navigate({
      to: "/c/$chatId",
      params: { chatId },
      search: searchFromLayout(next),
    });
  }

  return {
    layout,

    /** Show `panel` on the side it belongs, keeping whatever the other side holds. */
    openPanel(panel: PanelContent) {
      const side: PanelPosition = isContentPanel(panel) ? "right" : "left";
      const chat: PanelContent = { type: "chat" };
      const other =
        layout.mode === "split" ? (side === "left" ? layout.right : layout.left) : chat;

      let left = side === "left" ? panel : other;
      let right = side === "left" ? other : panel;
      // The slot we are not filling already holds this kind of panel; it has
      // been superseded, so hand that side back to the conversation.
      if (!isValidLayout(left, right)) {
        if (side === "left") right = chat;
        else left = chat;
      }
      go({ mode: "split", left, right, fullscreen: undefined });
    },

    /** Collapse back to the conversation alone. */
    closeAllPanels() {
      go({ mode: "single", panel: { type: "chat" } });
    },

    /**
     * Closing one slot returns the survivor to its own side, with the
     * conversation filling the slot it vacated. A lone conversation collapses
     * to the bare URL.
     */
    closePanel(position: PanelPosition) {
      if (layout.mode !== "split") {
        go({ mode: "single", panel: { type: "chat" } });
        return;
      }
      const survivor = position === "left" ? layout.right : layout.left;
      if (survivor.type === "chat") {
        go({ mode: "single", panel: { type: "chat" } });
        return;
      }
      const chat: PanelContent = { type: "chat" };
      go({
        mode: "split",
        left: isContentPanel(survivor) ? chat : survivor,
        right: isContentPanel(survivor) ? survivor : chat,
        fullscreen: undefined,
      });
    },

    toggleFullscreen(position: PanelPosition) {
      if (layout.mode !== "split") return;
      go({ ...layout, fullscreen: layout.fullscreen === position ? undefined : position });
    },
  };
}
