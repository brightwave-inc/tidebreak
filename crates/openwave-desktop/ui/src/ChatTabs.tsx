import { FileOutput, LibraryBig, MessageSquare } from "lucide-react";
import { useUiStore } from "./UiStore";

/**
 * Segmented control for a chat's per-conversation surfaces: the transcript
 * ("Chat") and its scoped Sources / Outputs. These views are keyed on the
 * selected chat, so the switcher lives in the chat headers rather than the
 * sidebar — the sidebar stays a list of chats, not a mix of global and
 * chat-scoped destinations.
 */
export function ChatTabs() {
  const primaryView = useUiStore((state) => state.primaryView);
  const showChat = useUiStore((state) => state.showChat);
  const showDocuments = useUiStore((state) => state.showDocuments);
  const showDeliverables = useUiStore((state) => state.showDeliverables);

  const tabs = [
    {
      key: "chat" as const,
      label: "Chat",
      icon: MessageSquare,
      onSelect: () => showChat({ keepPanels: true }),
    },
    {
      key: "documents" as const,
      label: "Sources",
      icon: LibraryBig,
      onSelect: showDocuments,
    },
    {
      key: "deliverables" as const,
      label: "Outputs",
      icon: FileOutput,
      onSelect: showDeliverables,
    },
  ];

  return (
    <div className="chat-tabs" role="tablist" aria-label="Chat views">
      {tabs.map((tab) => {
        const active = primaryView === tab.key;
        const Icon = tab.icon;
        return (
          <button
            key={tab.key}
            type="button"
            role="tab"
            aria-selected={active}
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
