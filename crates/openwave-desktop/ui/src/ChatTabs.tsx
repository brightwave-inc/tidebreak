import { FileOutput, FolderOpen, LibraryBig, MessageSquare } from "lucide-react";
import { useUiStore } from "./UiStore";
import type { SurfaceKind } from "./Surface";

/** Surfaces whose catalogs are served by the native host rather than the API. */
const NATIVE_HOST_SURFACES: ReadonlySet<SurfaceKind> = new Set([
  "documents",
  "deliverables",
  "folders",
]);

const UNAVAILABLE_HINT = "Available in the OpenWave desktop app";

export function requiresNativeHost(kind: SurfaceKind): boolean {
  return NATIVE_HOST_SURFACES.has(kind);
}

export type ChatTabsProps = {
  /** Native-host surfaces render disabled and explained when this is false. */
  nativeHost: boolean;
};

/**
 * Segmented control for a chat's per-conversation surfaces: the transcript
 * ("Chat") and its scoped Sources / Outputs / Folders. These views are keyed on
 * the selected chat, so the switcher lives in the chat workspace header rather
 * than the sidebar — the sidebar stays a list of chats, not a mix of global and
 * chat-scoped destinations.
 */
export function ChatTabs({ nativeHost }: ChatTabsProps) {
  const surface = useUiStore((state) => state.surface);
  const showChat = useUiStore((state) => state.showChat);
  const showDocuments = useUiStore((state) => state.showDocuments);
  const showDeliverables = useUiStore((state) => state.showDeliverables);
  const showFolders = useUiStore((state) => state.showFolders);

  const tabs = [
    {
      key: "chat" as const,
      label: "Chat",
      icon: MessageSquare,
      onSelect: showChat,
    },
    {
      key: "documents" as const,
      label: "Sources",
      icon: LibraryBig,
      onSelect: () => showDocuments(),
    },
    {
      key: "deliverables" as const,
      label: "Outputs",
      icon: FileOutput,
      onSelect: () => showDeliverables(),
    },
    {
      key: "folders" as const,
      label: "Folders",
      icon: FolderOpen,
      onSelect: showFolders,
    },
  ];

  return (
    <div className="chat-tabs" role="tablist" aria-label="Chat views">
      {tabs.map((tab) => {
        const unavailable = !nativeHost && requiresNativeHost(tab.key);
        const active = surface.kind === tab.key;
        const Icon = tab.icon;
        return (
          <button
            key={tab.key}
            type="button"
            role="tab"
            aria-selected={active}
            aria-disabled={unavailable || undefined}
            disabled={unavailable}
            title={unavailable ? UNAVAILABLE_HINT : undefined}
            className={`chat-tab${active ? " is-active" : ""}`}
            onClick={tab.onSelect}
          >
            <Icon size={14} />
            <span className="chat-tab-label">{tab.label}</span>
          </button>
        );
      })}
    </div>
  );
}
