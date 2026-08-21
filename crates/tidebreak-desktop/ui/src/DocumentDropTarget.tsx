import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";

import { attachDroppedChatFiles, type AttachedFiles } from "./attachments";
import { hasNativeHost } from "./host";

type DropPhase = "enter" | "leave" | "dropped";

type DropState = {
  phase: DropPhase;
  accepted: boolean;
  fileCount: number;
};

/**
 * Native file drops are intercepted by Tauri before the webview sees them.
 * This listener lives inside the composer and sends the claimed paths through
 * the same mixed attachment path as its paperclip button.
 *
 * The conversation arrives as a resolver rather than an id because the home
 * composer has no conversation until something is attached to it. The host
 * holds a dropped path set briefly, so creating one on the way through is
 * fine; what is not fine is refusing the drop for want of an id.
 */
export function DocumentDropTarget({
  resolveChatId,
  onAttached,
  onError,
}: {
  resolveChatId: () => Promise<string>;
  onAttached: (attached: AttachedFiles) => void;
  onError: (error: unknown) => void;
}) {
  const [state, setState] = useState<DropState | null>(null);
  const attachedRef = useRef(onAttached);
  const errorRef = useRef(onError);
  const resolveRef = useRef(resolveChatId);
  attachedRef.current = onAttached;
  errorRef.current = onError;
  resolveRef.current = resolveChatId;

  useEffect(() => {
    if (!hasNativeHost()) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listen<unknown>("library-import-drop-state", (event) => {
      const next = parseDropState(event.payload);
      if (!next || cancelled) return;
      if (next.phase === "leave") {
        setState(null);
        return;
      }
      if (next.phase === "dropped") {
        setState(null);
        if (next.accepted) {
          void (async () => {
            // Unmounting during this round trip cannot call the conversation
            // back: it is already created, and it shows up in the sidebar
            // unasked. Home writes the id into its draft, so the next send
            // from home reuses it rather than leaving it stranded.
            const chatId = await resolveRef.current();
            if (cancelled) return;
            const attached = await attachDroppedChatFiles(chatId);
            if (!cancelled && attached) attachedRef.current(attached);
          })().catch((error) => {
            if (!cancelled) errorRef.current(error);
          });
        }
        return;
      }
      setState(next);
    }).then((stop) => {
      if (cancelled) stop();
      else unlisten = stop;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  if (!state) return null;
  return (
    <div
      className={`absolute inset-0 z-20 flex flex-col items-center justify-center rounded-xl border-2 border-dashed bg-background/95 px-4 text-center ${state.accepted ? "border-primary" : "border-destructive"}`}
      aria-live="assertive"
    >
      <strong>
        {state.accepted
          ? `Attach ${dropItemCountCopy(state.fileCount)}`
          : "Only regular files and folders can be attached"}
      </strong>
      <span className="text-xs text-muted-foreground">
        {state.accepted
          ? "Release to add them to this message."
          : "Aliases and unavailable items cannot be imported."}
      </span>
    </div>
  );
}

export function parseDropState(value: unknown): DropState | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return null;
  }
  const state = value as Record<string, unknown>;
  if (
    !["enter", "leave", "dropped"].includes(String(state.phase)) ||
    typeof state.accepted !== "boolean" ||
    typeof state.fileCount !== "number" ||
    !Number.isSafeInteger(state.fileCount) ||
    state.fileCount < 0
  ) {
    return null;
  }
  return {
    phase: state.phase as DropPhase,
    accepted: state.accepted,
    fileCount: state.fileCount,
  };
}

export function dropItemCountCopy(count: number): string {
  return count === 1 ? "this file or folder" : `${count} files or folders`;
}
