import { mockIPC } from "@tauri-apps/api/mocks";
import type { Meta, StoryObj } from "@storybook/react-vite";
import type { Chat, ConsentStatementSnapshot } from "@/api";
import { FoldersView } from "@/FoldersView";
import type { ConnectedFolder, WidenedFolderCapability } from "@/host";

const chat = {
  id: "folder-story-chat",
  title: "Folder access story",
  project_id: null,
} as unknown as Chat;

type FolderStoryState = {
  connected: ConnectedFolder[];
  approved: ConnectedFolder[];
  consents: ConsentStatementSnapshot[];
  nextGrant: number;
};

function consent(
  grantId: string,
  rootId: string,
  capability: WidenedFolderCapability,
  method: ConsentStatementSnapshot["method"] = "folder_picker",
): ConsentStatementSnapshot {
  return {
    handle: { kind: "capability_grant", grant_id: grantId },
    level: { level: "chat", chat_id: chat.id },
    level_title: chat.title,
    verb: { kind: "capability", capability },
    resource: {
      kind: "host_root",
      root_id: rootId,
      display_name: null,
    },
    method,
    granted_at: "2026-08-25T16:00:00Z",
  };
}

function createFolderStoryState(): FolderStoryState {
  const connected: ConnectedFolder[] = [
    {
      rootId: "folder-product",
      displayName: "Product workspace",
      status: "connected",
      availableInFutureChats: true,
    },
    {
      rootId: "folder-archive",
      displayName: "External customer archive",
      status: "unavailable",
      availableInFutureChats: true,
    },
  ];
  return {
    connected,
    approved: [
      ...connected,
      {
        rootId: "folder-exports",
        displayName: "Client exports approved on this device",
        status: "connected",
        availableInFutureChats: false,
      },
    ],
    consents: [
      consent("grant-read", "folder-product", "read_files"),
      consent("grant-write", "folder-product", "write_files"),
      consent("grant-command", "folder-product", "execute_commands"),
    ],
    nextGrant: 1,
  };
}

function replaceFolderTrust(
  folders: ConnectedFolder[],
  rootId: string,
  trusted: boolean,
): ConnectedFolder[] {
  return folders.map((folder) =>
    folder.rootId === rootId
      ? { ...folder, availableInFutureChats: trusted }
      : folder,
  );
}

function installFolderStoryBackend() {
  const state = createFolderStoryState();
  mockIPC((command, args) => {
    const request = (args as { request?: Record<string, unknown> } | undefined)
      ?.request;
    switch (command) {
      case "list_connected_folders":
        return state.connected.map((folder) => ({ ...folder }));
      case "list_approved_folders":
        return state.approved.map((folder) => ({ ...folder }));
      case "list_capability_consents":
        return [...state.consents];
      case "set_trusted_folder": {
        const rootId = String(request?.rootId);
        const trusted = Boolean(request?.trusted);
        state.connected = replaceFolderTrust(state.connected, rootId, trusted);
        state.approved = replaceFolderTrust(state.approved, rootId, trusted);
        return true;
      }
      case "connect_approved_folder": {
        const rootId = String(request?.rootId);
        const approved = state.approved.find(
          (folder) => folder.rootId === rootId,
        );
        if (!approved) return null;
        const connected = { ...approved, status: "connected" as const };
        state.connected = [
          ...state.connected.filter((folder) => folder.rootId !== rootId),
          connected,
        ];
        return connected;
      }
      case "disconnect_folder": {
        const rootId = String(request?.rootId);
        state.connected = state.connected.filter(
          (folder) => folder.rootId !== rootId,
        );
        return true;
      }
      case "forget_folder": {
        const rootId = String(request?.rootId);
        state.connected = state.connected.filter(
          (folder) => folder.rootId !== rootId,
        );
        state.approved = state.approved.filter(
          (folder) => folder.rootId !== rootId,
        );
        state.consents = state.consents.filter(
          (statement) =>
            statement.resource.kind !== "host_root" ||
            statement.resource.root_id !== rootId,
        );
        return true;
      }
      case "grant_folder_capability": {
        const rootId = String(request?.rootId);
        const capability = request?.capability as WidenedFolderCapability;
        state.consents = [
          ...state.consents,
          consent(
            `grant-story-${state.nextGrant++}`,
            rootId,
            capability,
            "permission_dialog",
          ),
        ];
        return true;
      }
      case "revoke_capability_consent": {
        const grantId = String(request?.grantId);
        state.consents = state.consents.filter(
          (statement) =>
            statement.handle.kind !== "capability_grant" ||
            statement.handle.grant_id !== grantId,
        );
        return true;
      }
      case "connect_folder":
        return null;
      default:
        throw new Error(`Unexpected folder story command: ${command}`);
    }
  });
}

function renderFolderStory({ chat: storyChat }: { chat: Chat }) {
  installFolderStoryBackend();
  return <FoldersView chat={storyChat} />;
}

const meta = {
  title: "Chat/Folders",
  component: FoldersView,
  parameters: { layout: "fullscreen" },
  args: { chat },
  render: renderFolderStory,
} satisfies Meta<typeof FoldersView>;

export default meta;
type Story = StoryObj<typeof meta>;

export const FutureChatAccess: Story = {};

export const FutureChatAccessCompact: Story = {
  parameters: { viewport: { defaultViewport: "compact" } },
};
