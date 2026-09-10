import { useEffect } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { AppContextProvider, type AppContextValue } from "@/AppContext";
import type { ApiClient } from "@/api/client";
import type { SequencedCodeEventFrame, CodeSessionSnapshot } from "@/api/types";
import { CodeSessionContent } from "@/code/CodeSessionPage";
import { resetCodeSessionRegistry } from "@/code/CodeSessionRegistry";
import { codeSession } from "./fixtures";

const session: CodeSessionSnapshot = {
  ...codeSession,
  id: "slack-parent",
  workspace_id: null,
  harness_kind: "internal",
  access: "contribute",
  is_owner: false,
  lifecycle: "idle",
  external_origin: {
    channel_kind: "slack",
    external_key: "T1/C1/1789010035.271179",
  },
};
const client = {
  listCodeSessionTurns: async () => [],
  listCodeApprovals: async () => [],
  listCodeQueuedTurns: async () => ({ queued: [], paused: false }),
  listCodeHarnessModels: async () => ({ kind: "internal", models: [] }),
  openCodeEvents: (
    _session: string,
    _after: number,
    onFrame: (frame: SequencedCodeEventFrame) => void,
  ) => {
    const socket = {
      onopen: null as WebSocket["onopen"],
      close() {},
      addEventListener() {},
      removeEventListener() {},
    } as unknown as WebSocket;
    queueMicrotask(() => {
      socket.onopen?.(new Event("open"));
      onFrame({
        seq: 1,
        replayed: true,
        event: {
          type: "assistant_message",
          text: "The Slack adapter and Tidebreak changes are ready for review. The linked task opens the conversation that produced the result, including work across repositories.",
        },
      });
    });
    return socket;
  },
} as unknown as ApiClient;

function SessionStory({
  state,
}: {
  state: "conversation" | "view" | "loading" | "error";
}) {
  useEffect(() => () => resetCodeSessionRegistry(), []);
  return (
    <AppContextProvider value={{ client } as AppContextValue}>
      <div className="flex h-screen min-h-0 bg-background">
        <CodeSessionContent
          session={
            state === "loading" || state === "error"
              ? null
              : {
                  ...session,
                  ...(state === "view" ? { access: "view" as const } : {}),
                }
          }
          error={
            state === "error"
              ? "This conversation is unavailable. Check that you signed in to the right Tidebreak instance."
              : null
          }
          client={client}
          models={[]}
          defaultModelKey={null}
          onRetry={() => {}}
        />
      </div>
    </AppContextProvider>
  );
}
const meta = {
  title: "Code/Session link",
  component: SessionStory,
  parameters: { layout: "fullscreen" },
  args: { state: "conversation" },
} satisfies Meta<typeof SessionStory>;
export default meta;
type Story = StoryObj<typeof meta>;
export const Conversation: Story = {};
export const ViewAccess: Story = { args: { state: "view" } };
export const Loading: Story = { args: { state: "loading" } };
export const Unavailable: Story = { args: { state: "error" } };
