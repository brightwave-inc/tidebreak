import { toast } from "sonner";

import type { ApiClient, ConversationExportRequest } from "./api";
import { hasLocalHostAuthority, saveConversationExport } from "./host";
import { downloadBlob } from "./lib/downloadBlob";
import { friendlyErrorMessage } from "./lib/utils";

/**
 * Export one Work chat as Markdown, the way Settings → Data exports a chosen
 * few: through the native save dialog on this computer, as a download in a
 * browser.
 */
export async function exportChatConversation(
  client: Pick<ApiClient, "downloadConversationExport">,
  chatId: string,
  host: {
    local: boolean;
    save: typeof saveConversationExport;
  } = { local: hasLocalHostAuthority(), save: saveConversationExport },
): Promise<void> {
  const request: ConversationExportRequest = {
    format: "markdown",
    chat_ids: [chatId],
  };
  try {
    if (host.local) {
      const saved = await host.save(request);
      if (saved)
        toast.success("Exported this conversation", {
          description: saved.path,
        });
      return;
    }
    const file = await client.downloadConversationExport(request);
    downloadBlob(file.blob, file.fileName);
  } catch (error) {
    toast.error(
      friendlyErrorMessage(error, "Could not export this conversation."),
    );
  }
}
