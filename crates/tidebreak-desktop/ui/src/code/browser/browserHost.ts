import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { attachedRemotely, hasLocalHostAuthority, openExternal } from "@/host";
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

export type BrowserController =
  | {
      kind: "human";
      label?: string;
      action?: string;
      halted?: boolean;
      takeoverRequired?: boolean;
    }
  | {
      kind: "agent";
      label?: string;
      action?: string;
      halted?: boolean;
      takeoverRequired?: boolean;
    };

export type BrowserAgentAccess = {
  shared: boolean;
  paused: boolean;
  halted: boolean;
  origin?: string;
  scope?: "origin" | "loopback_workspace";
  canObserve: boolean;
  canControl: boolean;
  canTransferFiles: boolean;
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
  | { type: "share_with_agent" }
  | { type: "revoke_agent_access" }
  | { type: "stop_agent_control" }
  | { type: "take_human_control" }
  | { type: "set_inspect"; enabled: boolean }
  | { type: "remove_inspect" }
  | { type: "close" };

export type BrowserHostSnapshot = {
  exists: boolean;
  browserId: string;
  workspaceId: string;
  profileId?: string;
  url?: string;
  title?: string;
  loadState?: "loading" | "ready" | "failed";
  documentEpoch?: number;
  visible?: boolean;
  engine?: {
    name: "wk_webview" | "webview2" | "webkitgtk" | "unsupported";
    capabilities: {
      lifecycle: boolean;
      persistentProfile: boolean;
      semanticSnapshot: boolean;
      semanticActions: boolean;
      screenshot: boolean;
      crossOriginFrames: boolean;
      profileReset: boolean;
    };
  };
  controller?: BrowserController;
  agentAccess?: BrowserAgentAccess;
  inspectEnabled?: boolean;
};

export type BrowserHostEvent = {
  workspaceId: string;
  browserId: string;
  type:
    | "navigation_started"
    | "navigation_finished"
    | "same_document_navigation"
    | "title_changed"
    | "popup_blocked"
    | "download_blocked"
    | "navigation_blocked"
    | "controller_changed"
    | "agent_navigation_paused"
    | "agent_access_changed";
  url?: string;
  title?: string;
  message?: string;
  loadState?: "loading" | "ready" | "failed";
  documentEpoch?: number;
  controller?: BrowserController;
  agentAccess?: BrowserAgentAccess;
  origin?: string;
};

export type CodeBrowserHost = {
  available: () => boolean;
  command: (
    workspaceId: string,
    browserId: string,
    action: BrowserHostAction,
  ) => Promise<BrowserHostSnapshot>;
  subscribe: (
    handler: (event: BrowserHostEvent) => void,
  ) => Promise<() => void>;
  openExternal: (url: string) => Promise<void>;
};

export const nativeCodeBrowserHost: CodeBrowserHost = {
  /**
   * The browser is a child webview on this computer's screen, driven by this
   * computer's input. None of that belongs to a conversation on another
   * machine — sharing one with an agent that is not here shares the wrong
   * screen — so a window attached to a machine has no browser to offer.
   */
  available: hasLocalHostAuthority,
  command: async (workspaceId, browserId, action) => {
    const normalized = await logicalBrowserAction(action);
    return invoke<BrowserHostSnapshot>("code_browser_command", {
      request: { workspaceId, browserId, action: normalized },
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
    let browserZoomScale =
      cachedZoom?.cssViewportWidth === cssViewportWidth
        ? cachedZoom.browserZoomScale
        : undefined;
    if (browserZoomScale === undefined) {
      const current = getCurrentWindow();
      const [physical, scaleFactor] = await Promise.all([
        current.innerSize(),
        current.scaleFactor(),
      ]);
      const logicalWidth = physical.width / scaleFactor;
      browserZoomScale =
        cssViewportWidth > 0 ? logicalWidth / cssViewportWidth : 1;
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

/**
 * Why a browser tab cannot run here, for the tab that has to say so.
 *
 * A tab restored from a saved layout opens whether or not a browser can run,
 * so the reason has to be worded rather than assumed: a browser build has no
 * host at all, while an attached window has one that would open on the wrong
 * computer.
 */
export function browserUnavailableMessage(): string {
  return attachedRemotely()
    ? "The in-app browser runs on this computer, and your work is on another machine"
    : "The in-app browser is available in the Tidebreak desktop app";
}

/** Explicit tab close. Tab switches only hide the native child webview. */
export async function closeCodeBrowser(
  workspaceId: string,
  browserId: string,
  host: CodeBrowserHost = nativeCodeBrowserHost,
): Promise<void> {
  removeStoredBrowserSession(browserId);
  if (!host.available()) return;
  await host
    .command(workspaceId, browserId, { type: "close" })
    .catch(() => undefined);
}
