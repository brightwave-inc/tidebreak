import { Archive } from "lucide-react";

import type { Chat } from "./api";
import { useApp } from "./AppContext";
import { Button } from "@/components/ui/button";

/**
 * Says an open conversation is archived, and offers the way back.
 *
 * An archived conversation still opens and still takes messages: archiving
 * only moves its row. Sending brings it back to the list on its own, so the
 * notice says so rather than making the reader unarchive first.
 */
export function ArchivedChatNotice({ chat }: { chat: Chat }) {
  const { unarchiveChat } = useApp();
  return (
    <div
      role="status"
      className="notice-surface notice-info mx-auto mb-2 flex w-full max-w-3xl items-center gap-3 rounded-lg border px-3 py-2 text-sm"
    >
      <Archive
        aria-hidden="true"
        className="size-4 shrink-0 text-muted-foreground"
      />
      <p className="min-w-0 flex-1">
        This work is archived. It stays out of your list until you unarchive it
        or send a message.
      </p>
      <Button
        type="button"
        size="xs"
        variant="outline"
        onClick={() => unarchiveChat(chat)}
      >
        Unarchive
      </Button>
    </div>
  );
}
