import {
  createBrowserSession,
  type BrowserHistoryEntry,
  type BrowserSession,
} from "./browserSession";
import {
  browserDisplayAddress,
  validateBrowserUrl,
} from "./browserNavigation";

const STORAGE_KEY = "tidebreak.code-browser-sessions.v1";
const MAX_STORED_SESSIONS = 24;
const MAX_STORED_HISTORY = 50;

type StoredBrowserSessions = Record<string, BrowserSession>;

export function readStoredBrowserSession(
  browserId: string,
  storage: Pick<Storage, "getItem"> = window.localStorage,
): BrowserSession | null {
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return null;
    return parseBrowserSession(
      (parsed as Record<string, unknown>)[browserId],
      browserId,
    );
  } catch {
    return null;
  }
}

export function writeStoredBrowserSession(
  session: BrowserSession,
  storage: Pick<Storage, "getItem" | "setItem"> = window.localStorage,
): void {
  try {
    const raw = storage.getItem(STORAGE_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : {};
    const sessions: StoredBrowserSessions = {};
    if (parsed && typeof parsed === "object") {
      for (const [id, candidate] of Object.entries(
        parsed as Record<string, unknown>,
      )) {
        const valid = parseBrowserSession(candidate, id);
        if (valid) sessions[id] = valid;
      }
    }
    sessions[session.id] = persistableSession(session);
    const bounded = Object.fromEntries(
      Object.entries(sessions)
        .sort(([, left], [, right]) => right.updatedAt - left.updatedAt)
        .slice(0, MAX_STORED_SESSIONS),
    );
    storage.setItem(STORAGE_KEY, JSON.stringify(bounded));
  } catch {
    // Browser restoration is useful, but storage must never block navigation.
  }
}

export function removeStoredBrowserSession(
  browserId: string,
  storage: Pick<Storage, "getItem" | "setItem"> = window.localStorage,
): void {
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return;
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return;
    const sessions = { ...(parsed as Record<string, unknown>) };
    delete sessions[browserId];
    storage.setItem(STORAGE_KEY, JSON.stringify(sessions));
  } catch {
    // Best-effort cleanup.
  }
}

export function restoreOrCreateBrowserSession({
  browserId,
  workspaceId,
  initialUrl,
}: {
  browserId: string;
  workspaceId: string;
  initialUrl?: string;
}): BrowserSession {
  const restored = readStoredBrowserSession(browserId);
  if (restored?.workspaceId === workspaceId) return restored;
  const created = createBrowserSession({
    id: browserId,
    workspaceId,
    initialUrl,
  });
  writeStoredBrowserSession(created);
  return created;
}

/** Seed a newly allocated browser tab before the URL-backed panel mounts. */
export function seedBrowserSession({
  browserId,
  workspaceId,
  initialUrl,
}: {
  browserId: string;
  workspaceId: string;
  initialUrl?: string;
}): BrowserSession {
  const session = createBrowserSession({
    id: browserId,
    workspaceId,
    initialUrl,
  });
  writeStoredBrowserSession(session);
  return session;
}

/** Lightweight title lookup for center-tab rendering before the panel mounts. */
export function storedBrowserTitle(browserId: string): string {
  return readStoredBrowserSession(browserId)?.title || "Browser";
}

function persistableSession(session: BrowserSession): BrowserSession {
  const historyStart = Math.max(
    0,
    session.history.length - MAX_STORED_HISTORY,
  );
  const history = session.history.slice(historyStart);
  const shiftedIndex = session.historyIndex - historyStart;
  return {
    ...session,
    loadState: session.url ? "ready" : "idle",
    error: null,
    notice: null,
    history,
    historyIndex: history.length
      ? Math.min(Math.max(shiftedIndex, 0), history.length - 1)
      : -1,
  };
}

function parseBrowserSession(
  value: unknown,
  expectedId: string,
): BrowserSession | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  if (
    record.version !== 1 ||
    record.id !== expectedId ||
    typeof record.workspaceId !== "string" ||
    typeof record.title !== "string" ||
    typeof record.updatedAt !== "number" ||
    !Number.isFinite(record.updatedAt)
  ) {
    return null;
  }

  const history = parseHistory(record.history);
  const rawIndex =
    typeof record.historyIndex === "number" &&
    Number.isInteger(record.historyIndex)
      ? record.historyIndex
      : history.length - 1;
  const historyIndex = history.length
    ? Math.min(Math.max(rawIndex, 0), history.length - 1)
    : -1;
  const current = history[historyIndex];
  const rawUrl = typeof record.url === "string" ? record.url : current?.url;
  const target = rawUrl ? validateBrowserUrl(rawUrl) : null;
  const url = target?.ok ? target.url : null;

  return {
    version: 1,
    id: expectedId,
    workspaceId: record.workspaceId,
    url,
    address: url ? browserDisplayAddress(url) : "",
    title: record.title.slice(0, 160) || "Browser",
    loadState: url ? "ready" : "idle",
    error: null,
    notice: null,
    inspectEnabled: record.inspectEnabled === true,
    history,
    historyIndex,
    updatedAt: record.updatedAt,
  };
}

function parseHistory(value: unknown): BrowserHistoryEntry[] {
  if (!Array.isArray(value)) return [];
  const history: BrowserHistoryEntry[] = [];
  for (const candidate of value.slice(-MAX_STORED_HISTORY)) {
    if (!candidate || typeof candidate !== "object") continue;
    const record = candidate as Record<string, unknown>;
    if (typeof record.url !== "string") continue;
    const target = validateBrowserUrl(record.url);
    if (!target.ok) continue;
    history.push({
      url: target.url,
      title:
        typeof record.title === "string"
          ? record.title.replace(/\s+/g, " ").trim().slice(0, 160) || undefined
          : undefined,
    });
  }
  return history;
}
