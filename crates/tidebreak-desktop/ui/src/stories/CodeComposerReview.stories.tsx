import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, within } from "storybook/test";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { CodeComposer } from "@/code/CodeComposer";
import { usePendingReviewStore } from "@/code/diff/pendingReview";
import type { ReviewComment } from "@/code/diff/reviewComments";
import { useComposerDrafts } from "@/ComposerDrafts";

function app(): AppContextValue {
  return {
    client: {} as never,
    models: [],
    defaultModelKey: null,
    providers: [],
    refreshCatalog: async () => {},
    refreshChats: async () => {},
    status: "",
    setStatus: () => {},
    newChat: () => {},
    deleteChat: () => {},
    togglePinChat: () => {},
    archiveChat: () => {},
    unarchiveChat: () => {},
    startRename: () => {},
    commitRename: () => {},
    cancelRename: () => {},
    newProject: async () => false,
    deleteProject: () => {},
    startProjectRename: () => {},
    commitProjectRename: () => {},
    cancelProjectRename: () => {},
    newChatInProject: () => {},
    moveChatToProject: () => {},
    updateState: { status: "idle", version: null, error: null, enabled: false },
    updateUpToDate: false,
    checkForUpdate: async () => ({
      status: "idle",
      version: null,
      error: null,
      enabled: false,
    }),
    attachment: "local",
    restartForUpdate: async () => {},
  };
}

const WORKSPACE = "ws-composer-review";

function comment(id: string, path: string, body: string): ReviewComment {
  return {
    id,
    author: { kind: "person" },
    path,
    lines: [
      { kind: "add", oldNo: null, newNo: 18, text: "  message: string;" },
    ],
    body,
    createdAt: "2026-09-24T10:00:00.000Z",
  };
}

function seed(draft: string) {
  return async () => {
    const store = usePendingReviewStore.getState();
    store.clear(WORKSPACE);
    store.add(
      WORKSPACE,
      comment("r1", "src/code/sessionQueue.ts", "Keep the old field name."),
    );
    store.add(
      WORKSPACE,
      comment("r2", "src/code/sessionQueue.ts", "Why double the limit?"),
    );
    store.add(
      WORKSPACE,
      comment("r3", "crates/tidebreak-server/src/queue.rs", "Match this."),
    );
    useComposerDrafts.getState().setDraft("sess-review", draft);
    return {};
  };
}

function ComposerReviewStory() {
  return (
    <AppContextProvider value={app()}>
      <CodeComposer
        running={false}
        permissionMode="ask"
        sessionId="sess-review"
        reviewWorkspaceId={WORKSPACE}
        onSend={fn()}
        onInterrupt={fn()}
      />
    </AppContextProvider>
  );
}

/**
 * Comments left on the diff wait in the composer's context tray: how many,
 * across how many files. They go with the next message, and a message may
 * be the comments alone.
 */
const meta = {
  title: "Code/Composer review comments",
  component: ComposerReviewStory,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story) => (
      <div className="flex h-screen w-full flex-col justify-end">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof ComposerReviewStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Nothing typed: Send is live, because the comments are the message. */
export const CommentsOnly: Story = {
  loaders: [seed("")],
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByText("3 comments on 2 files"),
    ).resolves.toBeVisible();
    await expect(
      canvas.findByRole("button", { name: "Send message" }),
    ).resolves.toBeEnabled();
  },
};

export const CommentsWithAMessage: Story = {
  loaders: [seed("Fix these, then run the queue tests.")],
};
