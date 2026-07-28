import { useRouterState } from "@tanstack/react-router";

/**
 * The conversation the reader is in, or `null` anywhere else.
 *
 * The URL is the only record of this. Mirroring it into a store gave the shell
 * a second answer that outlived the route it came from, which is how home ended
 * up offering conversation-scoped controls that silently navigated back into
 * whichever chat had been open last.
 *
 * Read from the leaf match rather than with `useParams`, so this answers the
 * same thing in the shell — which sits above the chat route and would otherwise
 * see no params at all — as it does inside one.
 */
export function useActiveChatId(): string | null {
  return useRouterState({
    select: (state) => {
      const params = state.matches.at(-1)?.params as { chatId?: string } | undefined;
      return params?.chatId ?? null;
    },
  });
}
