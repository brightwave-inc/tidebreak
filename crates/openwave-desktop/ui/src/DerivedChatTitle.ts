import type { ApiClient, Chat } from "./api";

type DerivedTitleClient = Pick<ApiClient, "getChat">;

/**
 * Waits, in milliseconds, before each look for a server-derived chat title.
 *
 * The first is immediate: titling starts with the turn, so by the time the turn
 * resolves the name is usually already stored. The second covers the one-message
 * chat whose turn finished before the title did, and is the last — a title that
 * is not there by then is either still coming, in which case the next turn picks
 * it up, or was declined, in which case asking again would never help.
 */
export const DERIVED_TITLE_LOOKUPS = [0, 2_000];

/**
 * Look for a name the server derived for a chat nobody has named, and return the
 * chat to adopt once it has one.
 *
 * Titling runs beside a turn rather than inside it, so it produces no journal
 * event and the socket cannot report it — looking is the only way to see it. A
 * chat that already has a name is not asked about at all: the server never
 * replaces one, so there is nothing to find and a rename must not be raced by a
 * stale read of it.
 *
 * `null` means keep whatever is on screen. Nothing here is load-bearing enough
 * to report a failure over — the name is cosmetic, and the next turn looks again.
 */
export async function lookUpDerivedTitle(
  client: DerivedTitleClient,
  chatId: string,
  known: () => Chat | undefined,
  wait: (ms: number) => Promise<void>,
): Promise<Chat | null> {
  for (const delay of DERIVED_TITLE_LOOKUPS) {
    const current = known();
    if (!current || current.title !== null) return null;
    if (delay > 0) await wait(delay);
    try {
      const fetched = await client.getChat(chatId);
      if (fetched.title !== null) return fetched;
    } catch {
      return null;
    }
  }
  return null;
}
