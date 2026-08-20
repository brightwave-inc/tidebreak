import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { openExternal } from "@/host";
import { removeStoredBrowserSession } from "./browserPersistence";

export const CODE_BROWSER_EVENT = "code-browser:event";

let cachedZoom:
  | { cssViewportWidth: number; browserZoomScale: number }
  | undefined;

export type BrowserBounds = {
  x: number;
  y: number;
  width: number;
  height: number;
};

export type BrowserHostAction =
  | { type: "create"; url: string; bounds: BrowserBounds; visible: boolean }
  | { type: "navigate"; url: string }
  | { type: "reload" }
  | { type: "stop" }
  | { type: "back" }
  | { type: "forward" }
  | { type: "set_bounds"; bounds: BrowserBounds }
  | { type: "set_visible"; visible: boolean }
  | { type: "snapshot" }
  | { type: "close" };

export type BrowserHostSnapshot = {
  exists: boolean;
  url?: string;
};

export type BrowserHostEvent = {
  sessionId: string;
  type:
    | "navigation_started"
    | "navigation_finished"
    | "title_changed"
    | "popup_blocked"
    | "download_blocked"
    | "navigation_blocked";
  url?: string;
  title?: string;
  message?: string;
};

export type CodeBrowserHost = {
  available: () => boolean;
  command: (
    sessionId: string,
    action: BrowserHostAction,
  ) => Promise<BrowserHostSnapshot>;
  subscribe: (
    handler: (event: BrowserHostEvent) => void,
  ) => Promise<() => void>;
  openExternal: (url: string) => Promise<void>;
};

export const nativeCodeBrowserHost: CodeBrowserHost = {
  available: isTauri,
  command: async (sessionId, action) => {
    const normalized = await logicalBrowserAction(action);
    return invoke<BrowserHostSnapshot>("code_browser_command", {
      request: { sessionId, action: normalized },
    });
  },
  subscribe: async (handler) =>
    listen<BrowserHostEvent>(CODE_BROWSER_EVENT, (event) =>
      handler(event.payload),
    ),
  openExternal: async (url) => {
    if (!(await openExternal(url))) {
      window.open(url, "_blank", "noopener,noreferrer");
    }
  },
};

async function logicalBrowserAction(
  action: BrowserHostAction,
): Promise<BrowserHostAction> {
  if (action.type !== "create" && action.type !== "set_bounds") return action;

  try {
    const cssViewportWidth = window.innerWidth;
    let browserZoomScale = cachedZoom?.cssViewportWidth === cssViewportWidth
      ? cachedZoom.browserZoomScale
      : undefined;
    if (browserZoomScale === undefined) {
      const current = getCurrentWindow();
      const [physical, scaleFactor] = await Promise.all([
        current.innerSize(),
        current.scaleFactor(),
      ]);
      const logicalWidth = physical.width / scaleFactor;
      browserZoomScale = cssViewportWidth > 0
        ? logicalWidth / cssViewportWidth
        : 1;
      cachedZoom = { cssViewportWidth, browserZoomScale };
    }
    if (!Number.isFinite(browserZoomScale) || browserZoomScale <= 0) {
      return action;
    }
    return {
      ...action,
      bounds: {
        x: action.bounds.x * browserZoomScale,
        y: action.bounds.y * browserZoomScale,
        width: action.bounds.width * browserZoomScale,
        height: action.bounds.height * browserZoomScale,
      },
    };
  } catch {
    return action;
  }
}

/** Explicit tab close. Tab switches only hide the native child webview. */
export async function closeCodeBrowser(
  browserId: string,
  host: CodeBrowserHost = nativeCodeBrowserHost,
): Promise<void> {
  removeStoredBrowserSession(browserId);
  if (!host.available()) return;
  await host.command(browserId, { type: "close" }).catch(() => undefined);
}
