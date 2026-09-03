import { browserDisplayAddress, validateBrowserUrl } from "./browserNavigation";

const MAX_HISTORY_ENTRIES = 50;
const MAX_TITLE_CHARS = 160;

export type BrowserLoadState = "idle" | "loading" | "ready" | "failed";

export type BrowserHistoryEntry = {
  url: string;
  title?: string;
};

export type BrowserNotice = {
  kind: "popup" | "download" | "download_saved" | "blocked";
  url?: string;
  message: string;
};

export type BrowserSession = {
  version: 1;
  id: string;
  workspaceId: string;
  url: string | null;
  address: string;
  title: string;
  loadState: BrowserLoadState;
  error: string | null;
  notice: BrowserNotice | null;
  history: BrowserHistoryEntry[];
  historyIndex: number;
  inspectEnabled: boolean;
  updatedAt: number;
};

export function createBrowserSession({
  id,
  workspaceId,
  initialUrl,
  now = Date.now(),
}: {
  id: string;
  workspaceId: string;
  initialUrl?: string;
  now?: number;
}): BrowserSession {
  const target = initialUrl ? validateBrowserUrl(initialUrl) : null;
  const url = target?.ok ? target.url : null;
  return {
    version: 1,
    id,
    workspaceId,
    url,
    address: url ? browserDisplayAddress(url) : "",
    title: "Browser",
    loadState: url ? "loading" : "idle",
    error: target && !target.ok ? target.message : null,
    notice: null,
    history: url ? [{ url }] : [],
    historyIndex: url ? 0 : -1,
    inspectEnabled: false,
    updatedAt: now,
  };
}

export function beginBrowserNavigation(
  session: BrowserSession,
  url: string,
  now = Date.now(),
): BrowserSession {
  const current = session.history[session.historyIndex];
  const history =
    current?.url === url
      ? session.history
      : [...session.history.slice(0, session.historyIndex + 1), { url }].slice(
          -MAX_HISTORY_ENTRIES,
        );
  return {
    ...session,
    url,
    address: browserDisplayAddress(url),
    loadState: "loading",
    error: null,
    notice: null,
    history,
    historyIndex: history.length - 1,
    updatedAt: now,
  };
}

export function observeBrowserNavigation(
  session: BrowserSession,
  url: string,
  loadState: Extract<BrowserLoadState, "loading" | "ready">,
  now = Date.now(),
): BrowserSession {
  const current = session.history[session.historyIndex];
  if (current?.url === url) {
    return {
      ...session,
      url,
      address: browserDisplayAddress(url),
      loadState,
      error: null,
      updatedAt: now,
    };
  }
  return {
    ...beginBrowserNavigation(session, url, now),
    loadState,
  };
}

export function finishBrowserNavigation(
  session: BrowserSession,
  url: string,
  now = Date.now(),
): BrowserSession {
  return observeBrowserNavigation(session, url, "ready", now);
}

export function setBrowserTitle(
  session: BrowserSession,
  title: string,
  now = Date.now(),
): BrowserSession {
  const clean = cleanTitle(title);
  const history = session.history.map((entry, index) =>
    index === session.historyIndex
      ? { ...entry, title: clean || undefined }
      : entry,
  );
  return {
    ...session,
    title: clean || "Browser",
    history,
    updatedAt: now,
  };
}

export function moveBrowserHistory(
  session: BrowserSession,
  direction: -1 | 1,
  now = Date.now(),
): BrowserSession {
  const historyIndex = session.historyIndex + direction;
  const target = session.history[historyIndex];
  if (!target) return session;
  return {
    ...session,
    historyIndex,
    url: target.url,
    address: browserDisplayAddress(target.url),
    title: target.title || "Browser",
    loadState: "loading",
    error: null,
    notice: null,
    updatedAt: now,
  };
}

export function failBrowserSession(
  session: BrowserSession,
  error: string,
  now = Date.now(),
): BrowserSession {
  return {
    ...session,
    loadState: "failed",
    error,
    updatedAt: now,
  };
}

export function setBrowserNotice(
  session: BrowserSession,
  notice: BrowserNotice | null,
  now = Date.now(),
): BrowserSession {
  return { ...session, notice, updatedAt: now };
}

export function canBrowserGoBack(session: BrowserSession): boolean {
  return session.historyIndex > 0;
}

export function canBrowserGoForward(session: BrowserSession): boolean {
  return (
    session.historyIndex >= 0 &&
    session.historyIndex < session.history.length - 1
  );
}

function cleanTitle(title: string): string {
  return title.replace(/\s+/g, " ").trim().slice(0, MAX_TITLE_CHARS);
}
