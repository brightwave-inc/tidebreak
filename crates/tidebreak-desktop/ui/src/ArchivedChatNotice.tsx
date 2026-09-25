import { Archive } from "lucide-react";

import type { Chat } from "./api";
import { useApp } from "./AppContext";
import { Button } from "@/components/ui/button";
import { Notice } from "@/components/ui/notice";

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
    <Notice
      tone="info"
      icon={Archive}
      className="mx-auto mb-2 max-w-3xl"
      action={
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() => unarchiveChat(chat)}
        >
          Unarchive
        </Button>
      }
    >
      This conversation is archived. It stays out of your list until you
      unarchive it or send a message.
    </Notice>
  );
}
