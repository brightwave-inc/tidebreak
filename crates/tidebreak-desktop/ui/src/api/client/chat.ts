import {
  type AgentNotificationPage,
  type Chat,
  type ChatTranscript,
  type CompactionRun,
  type DocumentDetail,
  type ExecFileUndoOutcome,
  type FileDownloadProgress,
  inboxConversationKey,
  type InboxEntry,
  type ModelSelectionKey,
  type NetworkPolicy,
  type PendingChatPrompt,
  type PermissionMode,
  type ReasoningEffort,
} from "../types";
import { attachTrustedFolders } from "../../host";
import {
  parseAgentNotificationPage,
  parseInboxEntry,
  parsePendingChatPrompt,
} from "../parsers";
import { type Constructor, HttpCore } from "./http";

function parseMarked(body: unknown): number {
  if (
    !body ||
    typeof body !== "object" ||
    !("marked" in body) ||
    typeof (body as { marked: unknown }).marked !== "number"
  ) {
    throw new Error("notification mark response is invalid");
  }
  return (body as { marked: number }).marked;
}

/** Chats, notifications, the inbox, messages, attachments, and chat settings. */
export function withChatApi<TBase extends Constructor<HttpCore>>(Base: TBase) {
  return class extends Base {
    /**
     * Create a chat, optionally already set up the way it will run.
     *
     * The turn inputs are sent at creation rather than PATCHed afterwards: a
     * correcting PATCH races the first turn, which reads the chat as it was
     * created.
     */
    async createChat(
      model?: ModelSelectionKey,
      projectId?: string | null,
      settings?: {
        reasoningEffort?: ReasoningEffort | null;
        permissionMode?: PermissionMode | null;
        networkPolicy?: NetworkPolicy;
      },
    ): Promise<Chat> {
      const chat = await this.json<Chat>("/chats", {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify({
          model: model || undefined,
          project_id: projectId || undefined,
          reasoning_effort: settings?.reasoningEffort ?? undefined,
          permission_mode: settings?.permissionMode ?? undefined,
          network_policy: settings?.networkPolicy,
        }),
      });
      try {
        await attachTrustedFolders(chat);
      } catch (error) {
        // The chat already exists. Keep it usable if the local folder broker is
        // unavailable instead of making a retry create a second conversation.
        console.warn("could not attach saved folders to the new chat", error);
      }
      return chat;
    }

    listChats(): Promise<Chat[]> {
      return this.json("/chats", { headers: this.headers() });
    }

    getChat(chatId: string): Promise<Chat> {
      return this.json(`/chats/${chatId}`, { headers: this.headers() });
    }

    /**
     * Everything parked on this reader, across their conversations.
     *
     * One server-side read rather than a loop over chats: the shell needs the
     * whole set to badge the inbox and mark the rail, and asking each chat in
     * turn would make that cost grow with the profile.
     */
    async listNotifications(cursor?: string): Promise<AgentNotificationPage> {
      const query = cursor ? `?cursor=${encodeURIComponent(cursor)}` : "";
      const body = await this.json<unknown>(`/notifications${query}`, {
        headers: this.headers(),
      });
      const page = parseAgentNotificationPage(body);
      if (!page) {
        throw new Error("notification list response is invalid");
      }
      return page;
    }

    async notificationUnreadCount(): Promise<number> {
      const body = await this.json<unknown>("/notifications/unread-count", {
        headers: this.headers(),
      });
      if (
        !body ||
        typeof body !== "object" ||
        !("unread" in body) ||
        typeof (body as { unread: unknown }).unread !== "number"
      ) {
        throw new Error("notification unread-count response is invalid");
      }
      return (body as { unread: number }).unread;
    }

    async markNotificationsRead(ids: string[]): Promise<number> {
      const body = await this.json<unknown>("/notifications/read", {
        method: "POST",
        headers: this.headers(),
        body: JSON.stringify({ ids }),
      });
      return parseMarked(body);
    }

    async markAllNotificationsRead(): Promise<number> {
      const body = await this.json<unknown>("/notifications/read-all", {
        method: "POST",
        headers: this.headers(),
      });
      return parseMarked(body);
    }

    async listInbox(): Promise<InboxEntry[]> {
      const body = await this.json<unknown>("/inbox", {
        headers: this.headers(),
      });
      if (!Array.isArray(body)) {
        throw new Error("inbox response is not an array");
      }
      const entries: InboxEntry[] = [];
      const seen = new Set<string>();
      for (const value of body) {
        const entry = parseInboxEntry(value);
        if (!entry) {
          throw new Error("inbox response contains invalid data");
        }
        const key = inboxConversationKey(entry.conversation);
        if (seen.has(key)) {
          throw new Error("inbox response lists a conversation twice");
        }
        seen.add(key);
        entries.push(entry);
      }
      return entries;
    }

    async listPendingChatPrompts(): Promise<PendingChatPrompt[]> {
      const body = await this.json<unknown>("/chats/pending-prompts", {
        headers: this.headers(),
      });
      if (!Array.isArray(body)) {
        throw new Error("pending chat prompt response is not an array");
      }
      const prompts = new Map<string, PendingChatPrompt>();
      for (const item of body) {
        const prompt = parsePendingChatPrompt(item);
        if (!prompt || prompts.has(prompt.chatId)) {
          throw new Error("pending chat prompt response contains invalid data");
        }
        prompts.set(prompt.chatId, prompt);
      }
      return [...prompts.values()];
    }

    deleteChat(chatId: string): Promise<void> {
      return this.json(`/chats/${chatId}`, {
        method: "DELETE",
        headers: this.headers(),
      });
    }

    listChatMessages(chatId: string): Promise<ChatTranscript> {
      return this.json(`/chats/${chatId}/messages`, {
        headers: this.headers(),
      });
    }

    /**
     * Compact this chat now, optionally saying what the summary should keep.
     *
     * Runs between turns only; the server refuses while one is running. A
     * response with `compacted: false` is an ordinary answer — there was nothing
     * worth summarizing — not a failure.
     */
    compactChat(chatId: string, focus?: string): Promise<CompactionRun> {
      return this.json(`/chats/${encodeURIComponent(chatId)}/compact`, {
        method: "POST",
        headers: this.headers(true),
        body: JSON.stringify(focus ? { focus } : {}),
      });
    }

    undoTurnFileChanges(
      chatId: string,
      turnId: string,
    ): Promise<{
      chat_id: string;
      turn_id: string;
      files: ExecFileUndoOutcome[];
    }> {
      return this.json(
        `/chats/${encodeURIComponent(chatId)}/turns/${encodeURIComponent(turnId)}/file-changes/undo`,
        { method: "POST", headers: this.headers() },
      );
    }

    undoFileChange(
      chatId: string,
      turnId: string,
      snapshotId: string,
    ): Promise<ExecFileUndoOutcome> {
      return this.json(
        `/chats/${encodeURIComponent(chatId)}/turns/${encodeURIComponent(turnId)}/file-changes/${encodeURIComponent(snapshotId)}/undo`,
        { method: "POST", headers: this.headers() },
      );
    }

    getFileChangePreview(
      chatId: string,
      turnId: string,
      snapshotId: string,
      revision: "before" | "after",
      signal?: AbortSignal,
    ): Promise<Blob> {
      return this.blob(
        `/chats/${encodeURIComponent(chatId)}/turns/${encodeURIComponent(turnId)}/file-changes/${encodeURIComponent(snapshotId)}/preview/${revision}`,
        signal,
      );
    }

    getChatImageAttachment(
      chatId: string,
      attachmentId: string,
      signal?: AbortSignal,
    ): Promise<Blob> {
      return this.blob(
        `/chats/${encodeURIComponent(chatId)}/attachments/images/${encodeURIComponent(attachmentId)}`,
        signal,
      );
    }

    /**
     * Publish one file the renderer is already holding as a source on this chat.
     *
     * The native picker route reads the bytes in the host and imports them into
     * the store inside this app. That is the wrong store while this window works
     * on another machine, so a window in that state posts the bytes here instead
     * and the machine that owns the conversation parses them.
     *
     * The media type decides which parser runs. A browser leaves `File.type`
     * empty for a name it does not recognize, and the route refuses an empty
     * `Content-Type`, so an unrecognized file is declared as opaque bytes and
     * retained without extracted text rather than refused outright.
     */
    ingestChatDocument(
      chatId: string,
      file: File,
    ): Promise<{ document_id: string }> {
      return this.json(
        `/chats/${encodeURIComponent(chatId)}/documents/raw?title=${encodeURIComponent(file.name)}`,
        {
          method: "POST",
          headers: {
            Authorization: `Bearer ${this.token}`,
            "Content-Type": file.type || "application/octet-stream",
          },
          body: file,
        },
      );
    }

    /** One source's extracted text and catalog metadata. */
    getChatDocument(
      chatId: string,
      documentId: string,
    ): Promise<DocumentDetail> {
      return this.json(
        `/chats/${encodeURIComponent(chatId)}/documents/${encodeURIComponent(documentId)}`,
        { headers: this.headers() },
      );
    }

    /**
     * The original bytes of one source, exactly as they were imported.
     *
     * Bytes are addressed by document id inside its conversation and never by a
     * host path, so a viewer can show the file the reader gave us without the
     * renderer learning where on disk it came from. The conversation is part of
     * the address rather than decoration: the server serves a document's bytes
     * only under the chat that owns it.
     *
     * Returned as bytes rather than a URL because the renderer authenticates with
     * a bearer header the webview cannot attach to an `<embed>` or `<img>` source,
     * and because pdf.js and the workbook parsers want a buffer anyway. The
     * stored media type comes back alongside them because it is what the text
     * viewers dispatch on, and it would otherwise be lost when the streamed
     * chunks are reassembled.
     */
    getChatDocumentFile(
      chatId: string,
      documentId: string,
      signal?: AbortSignal,
      onProgress?: (progress: FileDownloadProgress) => void,
    ): Promise<{ bytes: Uint8Array; contentType: string | null }> {
      return this.streamBytes(
        `/chats/${encodeURIComponent(chatId)}/documents/${encodeURIComponent(documentId)}/file-content`,
        signal,
        onProgress,
      );
    }

    patchChatTitle(chatId: string, title: string | null): Promise<Chat> {
      return this.json(`/chats/${chatId}`, {
        method: "PATCH",
        headers: this.headers(true),
        body: JSON.stringify({ title }),
      });
    }

    patchChatModel(
      chatId: string,
      model: ModelSelectionKey | null,
    ): Promise<Chat> {
      return this.json(`/chats/${chatId}`, {
        method: "PATCH",
        headers: this.headers(true),
        body: JSON.stringify({ model }),
      });
    }

    patchChatReasoningEffort(
      chatId: string,
      reasoningEffort: ReasoningEffort | null,
    ): Promise<Chat> {
      return this.json(`/chats/${chatId}`, {
        method: "PATCH",
        headers: this.headers(true),
        body: JSON.stringify({ reasoning_effort: reasoningEffort }),
      });
    }

    patchChatPermissionMode(
      chatId: string,
      permissionMode: PermissionMode | null,
    ): Promise<Chat> {
      return this.json(`/chats/${chatId}`, {
        method: "PATCH",
        headers: this.headers(true),
        body: JSON.stringify({ permission_mode: permissionMode }),
      });
    }

    /**
     * File a chat under a project, or take it back out with `null`.
     *
     * The server refuses (409) a chat that still holds connected folders: its
     * folder grants are keyed to the identity it would be leaving.
     */
    moveChatToProject(chatId: string, projectId: string | null): Promise<Chat> {
      return this.json(`/chats/${encodeURIComponent(chatId)}`, {
        method: "PATCH",
        headers: this.headers(true),
        body: JSON.stringify({ project_id: projectId }),
      });
    }

    patchChatNetworkPolicy(
      chatId: string,
      networkPolicy: NetworkPolicy,
    ): Promise<Chat> {
      return this.json(`/chats/${chatId}`, {
        method: "PATCH",
        headers: this.headers(true),
        body: JSON.stringify({ network_policy: networkPolicy }),
      });
    }
  };
}
