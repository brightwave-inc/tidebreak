import { create } from "zustand";

import type {
  ApiClient,
  ModelSelectionKey,
  NetworkPolicy,
  PermissionMode,
  ReasoningEffort,
  StickyChatDefaults,
} from "./api";

/**
 * What the next chat will be created with, chosen before it exists.
 *
 * The home composer carries the same controls as a conversation's, but there
 * is nothing to PATCH yet: the choices are held here and passed to `POST
 * /chats`, so a chat starts the way it was set up rather than being created
 * one way and corrected a moment later. That correcting PATCH is the failure
 * this avoids — it races the first turn, which reads the chat as it was
 * created.
 *
 * Persistence lives on the server, not here. Every explicit choice — at these
 * pickers or inside a chat — is recorded by the server as a sticky default,
 * and an unspecified `POST /chats` field seeds from it. So this store holds
 * only this visit's explicit picks (`null` follows the default) plus the
 * server-reported defaults for display, refreshed on each home mount so a
 * choice made inside a chat shows up when the reader comes back.
 */
type NewChatSettings = {
  /** Server-recorded sticky defaults; `null` until the first load lands. */
  defaults: StickyChatDefaults | null;
  model: ModelSelectionKey | null;
  reasoningEffort: ReasoningEffort | null;
  permissionMode: PermissionMode | null;
  networkPolicy: NetworkPolicy | null;
  setModel: (model: ModelSelectionKey | null) => void;
  setReasoningEffort: (effort: ReasoningEffort | null) => void;
  setPermissionMode: (mode: PermissionMode) => void;
  setNetworkPolicy: (policy: NetworkPolicy) => void;
  /**
   * Refresh the server-side defaults. Best-effort: a failed read leaves the
   * previous defaults standing — the server still seeds the create correctly,
   * the pickers just display the hard defaults until a read lands.
   */
  loadDefaults: (client: ApiClient) => Promise<void>;
};

export const useNewChatSettings = create<NewChatSettings>((set) => ({
  defaults: null,
  model: null,
  reasoningEffort: null,
  permissionMode: null,
  networkPolicy: null,
  setModel: (model) => set({ model }),
  setReasoningEffort: (reasoningEffort) => set({ reasoningEffort }),
  setPermissionMode: (permissionMode) => set({ permissionMode }),
  setNetworkPolicy: (networkPolicy) => set({ networkPolicy }),
  loadDefaults: async (client) => {
    try {
      const settings = await client.getSettings();
      set({ defaults: settings.chat_defaults });
    } catch {
      // Keep whatever was displayed; creation seeds server-side regardless.
    }
  },
}));

/**
 * What the pickers display and the next chat will get: this visit's explicit
 * pick, else the server's sticky default, else the hard default the server
 * would fall back to (`ask`, open network, the configured model).
 *
 * The model stays a `ModelSelectionKey` cast because the server reports the
 * raw sticky selection; one whose provider was since removed reads as
 * "unavailable" in the picker, exactly the way it does inside a chat.
 */
export function effectiveNewChatSettings(state: NewChatSettings): {
  model: ModelSelectionKey | null;
  reasoningEffort: ReasoningEffort | null;
  permissionMode: PermissionMode | null;
  networkPolicy: NetworkPolicy;
} {
  return {
    model:
      state.model ??
      (state.defaults?.model as ModelSelectionKey | null) ??
      null,
    reasoningEffort:
      state.reasoningEffort ?? state.defaults?.reasoning_effort ?? null,
    permissionMode:
      state.permissionMode ?? state.defaults?.permission_mode ?? null,
    networkPolicy: state.networkPolicy ??
      state.defaults?.network_policy ?? { mode: "open" },
  };
}
