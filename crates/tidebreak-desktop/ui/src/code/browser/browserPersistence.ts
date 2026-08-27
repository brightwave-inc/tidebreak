import { validateBrowserUrl } from "./browserNavigation";

export const LEGACY_BROWSER_STORAGE_KEY = "tidebreak.code-browser-sessions.v1";

export type LegacyBrowserState = {
  version: 1;
  id: string;
  workspaceId: string;
  url: string | null;
  title: string | null;
};

export type LegacyBrowserStateRead =
  | { kind: "valid"; state: LegacyBrowserState }
  | { kind: "invalid"; state: null };

/** Read one renderer-owned session only for the native one-time migration. */
export function readLegacyBrowserSession(
  browserId: string,
  storage: Pick<Storage, "getItem"> = window.localStorage,
): LegacyBrowserStateRead | null {
  let raw: string | null;
  try {
    raw = storage.getItem(LEGACY_BROWSER_STORAGE_KEY);
  } catch {
    return null;
  }
  if (!raw) return null;

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return { kind: "invalid", state: null };
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { kind: "invalid", state: null };
  }
  const sessions = parsed as Record<string, unknown>;
  if (!Object.hasOwn(sessions, browserId)) return null;
  const state = parseLegacyBrowserState(sessions[browserId], browserId);
  return state ? { kind: "valid", state } : { kind: "invalid", state: null };
}

/** Clear one legacy entry only after native code acknowledges the migration. */
export function clearLegacyBrowserSession(
  browserId: string,
  storage: Pick<
    Storage,
    "getItem" | "setItem" | "removeItem"
  > = window.localStorage,
): void {
  let raw: string | null;
  try {
    raw = storage.getItem(LEGACY_BROWSER_STORAGE_KEY);
  } catch {
    return;
  }
  if (!raw) return;

  try {
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      storage.removeItem(LEGACY_BROWSER_STORAGE_KEY);
      return;
    }
    const sessions = { ...(parsed as Record<string, unknown>) };
    delete sessions[browserId];
    if (Object.keys(sessions).length === 0) {
      storage.removeItem(LEGACY_BROWSER_STORAGE_KEY);
    } else {
      storage.setItem(LEGACY_BROWSER_STORAGE_KEY, JSON.stringify(sessions));
    }
  } catch {
    storage.removeItem(LEGACY_BROWSER_STORAGE_KEY);
  }
}

function parseLegacyBrowserState(
  value: unknown,
  expectedId: string,
): LegacyBrowserState | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
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

  let url: string | null = null;
  if (record.url !== null && record.url !== undefined) {
    if (typeof record.url !== "string") return null;
    const target = validateBrowserUrl(record.url);
    if (!target.ok) return null;
    url = target.url;
  }

  return {
    version: 1,
    id: expectedId,
    workspaceId: record.workspaceId,
    url,
    title: record.title || null,
  };
}
