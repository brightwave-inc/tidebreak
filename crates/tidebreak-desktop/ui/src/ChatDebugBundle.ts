import { toast } from "sonner";

import { copyPlainText } from "./ClipboardCopyButton";
import { copyChatDebugBundle, saveChatDebugBundle } from "./host";
import {
  PICKER_BUSY_MESSAGE,
  PICKER_HOLDERS,
  useNativePickerLatch,
} from "./NativePickerLatch";

/**
 * "Copy debug info" / "Save debug bundle…" — the affordance a reader uses to
 * attach a complete diagnostic to a bug report.
 *
 * The document itself is built natively from the event journal
 * (`crates/tidebreak-desktop/src/chat_debug.rs`); this module is only the
 * gesture, the clipboard write, and the notice. The notice matters as much as
 * the bundle: the export deliberately carries the whole conversation, so the
 * reader has to be told that before they paste it into a public issue.
 */

/** What the reader is told after the action, on the surface that ran it. */
export type DebugNotice = {
  message: string;
  description?: string;
  error?: boolean;
};

export type ChatDebugDeps = {
  buildBundle: (chatId: string) => Promise<string>;
  saveBundle: (chatId: string) => Promise<boolean>;
  writeClipboard: (text: string) => Promise<void>;
  notify: (notice: DebugNotice) => void;
  claimPicker: (holder: string) => boolean;
  releasePicker: (holder: string) => void;
};

/**
 * Stated on every success. The bundle strips credential-shaped tokens and
 * nothing else — saying "redacted" without saying what survives would be the
 * dishonest version of this message.
 */
export const DEBUG_CONTENTS_NOTICE =
  "Includes the full conversation, tool arguments and results, and file paths. " +
  "API keys and similar tokens are removed. Review it before sharing.";

export function chatDebugDeps(): ChatDebugDeps {
  return {
    buildBundle: copyChatDebugBundle,
    saveBundle: saveChatDebugBundle,
    writeClipboard: (text) => copyPlainText(text),
    notify: ({ message, description, error }) => {
      if (error) toast.error(message, { description });
      else toast.success(message, { description });
    },
    claimPicker: (holder) => useNativePickerLatch.getState().claim(holder),
    releasePicker: (holder) => useNativePickerLatch.getState().release(holder),
  };
}

export async function copyChatDebug(
  chatId: string,
  deps: ChatDebugDeps,
): Promise<void> {
  try {
    const bundle = await deps.buildBundle(chatId);
    await deps.writeClipboard(bundle);
    deps.notify({
      message: "Debug info copied",
      description: DEBUG_CONTENTS_NOTICE,
    });
  } catch {
    deps.notify({
      message: "Could not copy debug info",
      description: "Try “Save debug bundle…” instead.",
      error: true,
    });
  }
}

export async function saveChatDebug(
  chatId: string,
  deps: ChatDebugDeps,
): Promise<void> {
  if (!deps.claimPicker(PICKER_HOLDERS.saveDebugBundle)) {
    deps.notify({ message: PICKER_BUSY_MESSAGE, error: true });
    return;
  }
  try {
    // Unlike the clipboard document, the saved file is untruncated, so this is
    // the path to point a reader at when a chat is too large to copy.
    if (await deps.saveBundle(chatId)) {
      deps.notify({
        message: "Debug bundle saved",
        description: DEBUG_CONTENTS_NOTICE,
      });
    }
  } catch {
    deps.notify({ message: "Could not save the debug bundle", error: true });
  } finally {
    deps.releasePicker(PICKER_HOLDERS.saveDebugBundle);
  }
}
