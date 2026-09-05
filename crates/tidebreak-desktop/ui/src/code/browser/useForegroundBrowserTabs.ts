import type { LayoutState, PanelContent } from "@/panel/panelTypes";
import { useBrowserTabs } from "../workspace/useBrowserTabs";
import { foregroundBrowserScope } from "./foregroundBrowserScope";

/** Keep each chat's browser tabs across navigation and restart. */
export function useForegroundBrowserTabs({
  chatId,
  layout,
  setLayout,
  openPanel,
}: {
  chatId: string;
  layout: LayoutState;
  setLayout: (next: LayoutState) => void;
  openPanel: (panel: PanelContent) => void;
}) {
  const browsers = useBrowserTabs({
    workspaceId: foregroundBrowserScope(chatId),
    layout,
    setLayout,
  });

  function openBrowser(url?: string) {
    if (!url) {
      const existing = layout.tabs.find((tab) => tab.type === "browser");
      if (existing) {
        openPanel(existing);
        return;
      }
    }
    browsers.openBrowser(url, "primary");
  }

  return { ...browsers, openBrowser };
}
