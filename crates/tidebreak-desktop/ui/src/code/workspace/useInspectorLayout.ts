import { useMemo } from "react";
import { useDefaultLayout, useGroupRef } from "react-resizable-panels";
import { useCodeUiStore } from "../CodeUiStore";
import {
  DEFAULT_INSPECTOR_LAYOUT,
  fitsInspectorSplit,
  INSPECTOR_LAYOUT_STORAGE_ID,
  INSPECTOR_PANEL_IDS,
  usableInspectorLayout,
} from "../inspectorLayout";
import { useMeasuredWidth } from "./layout";

/**
 * The review sidebar beside a workspace: the split sizes the reader last
 * saved, whether the pane is wide enough to show it, and the store actions
 * that open and close it.
 */
export function useInspectorLayout() {
  const inspectorLayout = useDefaultLayout({
    id: INSPECTOR_LAYOUT_STORAGE_ID,
    panelIds: INSPECTOR_PANEL_IDS,
    onlySaveAfterUserInteractions: true,
  });
  const inspectorGroupRef = useGroupRef();
  const reviewSidebarOpen = useCodeUiStore((state) => state.reviewSidebarOpen);
  const toggleReviewSidebar = useCodeUiStore(
    (state) => state.toggleReviewSidebar,
  );
  const setReviewSidebarOpen = useCodeUiStore(
    (state) => state.setReviewSidebarOpen,
  );

  const inspectorDefaultLayout = useMemo(
    () =>
      usableInspectorLayout(inspectorLayout.defaultLayout) ?? {
        ...DEFAULT_INSPECTOR_LAYOUT,
      },
    [inspectorLayout.defaultLayout],
  );

  const { paneRef: inspectorPaneRef, width: inspectorPaneWidth } =
    useMeasuredWidth();
  const inspectorFits = fitsInspectorSplit(inspectorPaneWidth);
  /**
   * The split only appears when the reader asked for it and the pane can
   * carry it. The stored preference survives a narrow window, so widening
   * one brings the inspector straight back.
   */
  const inspectorOpen = reviewSidebarOpen && inspectorFits;

  return {
    inspectorLayout,
    inspectorGroupRef,
    inspectorDefaultLayout,
    inspectorPaneRef,
    inspectorFits,
    inspectorOpen,
    toggleReviewSidebar,
    setReviewSidebarOpen,
  };
}
