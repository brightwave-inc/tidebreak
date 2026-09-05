import type { LayoutState } from "@/panel/panelTypes";
import {
  type CodeEditorRegion,
  codeBrowserIds,
  openCodeEditor,
  removedCodeBrowserIds,
} from "../codeChrome";
import { attachedRemotely } from "@/host";
import { browserTitlesForLayout } from "./layout";
import {
  readBrowserTabLayout,
  writeBrowserTabLayout,
} from "./browserTabLayout";
import { closeCodeBrowser } from "../browser/browserHost";
import { seedBrowserSession } from "../browser/browserPersistence";
import { useCallback, useEffect, useRef, useState } from "react";

/**
 * The browser tabs one workspace has open: their titles, the address each
 * started on, and the native webview behind each.
 *
 * Tab membership survives workspace navigation and restart. Each panel hides
 * its native view on unmount; only removing its tab closes the native session.
 */
export function useBrowserTabs({
  workspaceId,
  layout,
  setLayout,
}: {
  workspaceId: string;
  layout: LayoutState;
  setLayout: (next: LayoutState) => void;
}) {
  const previousLayoutRef = useRef(layout);
  const closedBrowserIdsRef = useRef(new Set<string>());
  const restorationCheckedRef = useRef(false);
  const restoringRef = useRef(false);
  const [browserTitles, setBrowserTitles] = useState<Record<string, string>>(
    () => browserTitlesForLayout(layout),
  );
  const [browserInitialUrls, setBrowserInitialUrls] = useState<
    Record<string, string>
  >({});

  const closeBrowserPanels = useCallback(
    (browserIds: readonly string[]) => {
      for (const browserId of browserIds) {
        if (closedBrowserIdsRef.current.has(browserId)) continue;
        closedBrowserIdsRef.current.add(browserId);
        void closeCodeBrowser(workspaceId, browserId);
      }
    },
    [workspaceId],
  );

  useEffect(() => {
    const empty = layout.tabs.length === 0 && !layout.editorSplit?.tabs.length;
    if (!restorationCheckedRef.current) {
      restorationCheckedRef.current = true;
      if (empty && !attachedRemotely()) {
        const saved = readBrowserTabLayout(workspaceId);
        if (codeBrowserIds(saved).length > 0) {
          restoringRef.current = true;
          setLayout(saved);
          return;
        }
      }
    }
    // Router navigation is asynchronous, including Strict Mode effect replay.
    // Keep the saved tabs until the restored URL reaches this hook.
    if (restoringRef.current && empty) return;
    restoringRef.current = false;
    const ids = codeBrowserIds(layout);
    for (const browserId of ids) {
      closedBrowserIdsRef.current.delete(browserId);
    }
    closeBrowserPanels(
      removedCodeBrowserIds(previousLayoutRef.current, layout),
    );
    previousLayoutRef.current = layout;
    if (!attachedRemotely()) writeBrowserTabLayout(workspaceId, layout);
    setBrowserTitles((current) => {
      let changed = false;
      const next: Record<string, string> = {};
      for (const browserId of ids) {
        const title = current[browserId] ?? "Browser";
        next[browserId] = title;
        if (current[browserId] !== title) changed = true;
      }
      if (Object.keys(current).length !== ids.length) changed = true;
      return changed ? next : current;
    });
  }, [closeBrowserPanels, layout, setLayout, workspaceId]);

  function openBrowser(url?: string, preferredRegion?: CodeEditorRegion) {
    const browserId = crypto.randomUUID();
    seedBrowserSession({
      browserId,
      workspaceId,
      initialUrl: url,
    });
    if (url) {
      setBrowserInitialUrls((current) => ({
        ...current,
        [browserId]: url,
      }));
    }
    setBrowserTitles((current) => ({
      ...current,
      [browserId]: "Browser",
    }));
    setLayout(
      openCodeEditor(layout, { type: "browser", browserId }, preferredRegion),
    );
  }

  /** The page behind a tab reported its document title. */
  function setBrowserTitle(browserId: string, title: string) {
    setBrowserTitles((current) =>
      current[browserId] === title
        ? current
        : { ...current, [browserId]: title },
    );
  }

  // The browser opens as a child webview on this computer. A window working on
  // another machine has no such screen to lend — sharing one with an agent that
  // is not here shares the wrong browser — so the row is absent rather than
  // present and refusing.
  const canNewBrowser = !attachedRemotely();

  return {
    browserTitles,
    browserInitialUrls,
    openBrowser,
    setBrowserTitle,
    canNewBrowser,
  };
}
