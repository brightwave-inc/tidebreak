import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, userEvent, within } from "storybook/test";

import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { CodeComposer } from "@/code/CodeComposer";
import { claudeSlashCommands } from "./fixtures";

/**
 * Code-mode `/` popup. Empty or absent command lists hide it; a probe that
 * captured engine commands shows them. Free-typed `/` text still submits
 * verbatim either way.
 */
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

function ComposerSlashStory({
  slashCommands,
}: {
  slashCommands?: typeof claudeSlashCommands;
}) {
  return (
    <AppContextProvider value={app()}>
      <CodeComposer
        running={false}
        permissionMode="ask"
        slashCommands={slashCommands}
        onSend={fn()}
        onInterrupt={fn()}
      />
    </AppContextProvider>
  );
}

const meta = {
  title: "Code/Composer slash",
  component: ComposerSlashStory,
  parameters: { layout: "fullscreen" },
  decorators: [
    (Story) => (
      <div className="flex h-screen w-full flex-col justify-end">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof ComposerSlashStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/** No engine listing: `/` stays in the draft and the popup stays hidden. */
export const HiddenWhenEmpty: Story = {
  args: { slashCommands: [] },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const box = await canvas.findByRole("textbox", { name: "Message" });
    await userEvent.click(box);
    await userEvent.keyboard("/");
    await expect(canvas.queryByRole("listbox")).toBeNull();
  },
};

/** A probe that returned commands feeds the popup. */
export const Populated: Story = {
  args: { slashCommands: claudeSlashCommands },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const box = await canvas.findByRole("textbox", { name: "Message" });
    await userEvent.click(box);
    await userEvent.keyboard("/");
    await expect(await canvas.findByRole("listbox")).toBeVisible();
    await expect(
      canvas.getByRole("option", { name: /compact/i }),
    ).toBeVisible();
  },
};
