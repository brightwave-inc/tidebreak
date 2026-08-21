/** Canonical prefix for foreground-chat browser workspace-scope strings. */
const FOREGROUND_PREFIX = "foreground-chat:" as const;

/**
 * Derive the browser workspace-scope string for a foreground chat.
 *
 * The returned value is a deterministic opaque scope string.  The desktop
 * native executor derives the same value from the persisted chat id, so
 * the renderer must never invent or alter the mapping.
 */
export function foregroundBrowserScope(chatId: string): string {
  return `${FOREGROUND_PREFIX}${chatId}`;
}
