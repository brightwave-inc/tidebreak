import { useMemo } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, waitFor, within } from "storybook/test";

import { encodeTerminalBytes, TerminalPane } from "@/code/TerminalPane";

type TerminalScenario = "write-failure" | "attach-failure" | "read-failure";

const terminal = {
  id: "terminal-story",
  workspace_id: "workspace-story",
  cols: 80,
  rows: 24,
  ended: false,
  created_at: "2026-08-25T18:00:00.000Z",
};

function TerminalRecoveryStory({ scenario }: { scenario: TerminalScenario }) {
  const client = useMemo(() => storyClient(scenario), [scenario]);

  return (
    <div className="grid min-h-dvh place-items-center bg-muted/45 p-4 sm:p-8">
      <div className="flex h-[min(620px,calc(100dvh-2rem))] min-h-[360px] w-full max-w-5xl flex-col overflow-hidden rounded-xl border border-border-subtle bg-background sm:h-[min(620px,calc(100dvh-4rem))]">
        <TerminalPane client={client} workspaceId="workspace-story" />
      </div>
    </div>
  );
}

function storyClient(scenario: TerminalScenario) {
  let reads = 0;
  return {
    listCodeTerminals: async () => {
      if (scenario === "attach-failure") {
        throw new Error("The terminal service did not answer");
      }
      return [terminal];
    },
    createCodeTerminal: async () => terminal,
    readCodeTerminal: async () => {
      if (scenario === "read-failure") {
        throw new Error("Terminal output stopped updating");
      }
      const firstRead = reads === 0;
      reads += 1;
      return {
        id: terminal.id,
        workspace_id: terminal.workspace_id,
        bytes: firstRead
          ? encodeTerminalBytes(
              "Last login: Tue Aug 25 17:58:14\r\nthet@tidebreak % pnpm test\r\n",
            )
          : "",
        cursor: 62,
        overflow: false,
        truncated: false,
        ended: false,
      };
    },
    writeCodeTerminal: async () => {
      if (scenario === "write-failure") {
        await new Promise((resolve) => setTimeout(resolve, 250));
        throw new Error(
          "The terminal connection dropped before it confirmed the input",
        );
      }
    },
    resizeCodeTerminal: async () => terminal,
  };
}

const meta = {
  title: "Code/Terminal recovery",
  component: TerminalRecoveryStory,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof TerminalRecoveryStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const WriteFailure: Story = {
  args: { scenario: "write-failure" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await waitFor(() =>
      expect(canvas.getByTestId("terminal-host")).toHaveAttribute(
        "aria-disabled",
        "false",
      ),
    );
    const input = canvasElement.querySelector<HTMLTextAreaElement>(
      ".xterm-helper-textarea",
    );
    if (!input) throw new Error("xterm input did not mount");
    input.focus();
    await userEvent.keyboard("pnpm test{Enter}");
    await expect(
      await canvas.findByTestId("terminal-write-failure"),
    ).toBeVisible();
    await expect(canvas.getByRole("button", { name: "Retry" })).toBeVisible();
    await expect(
      canvas.getByRole("button", { name: "Reconnect" }),
    ).toBeVisible();
    await expect(canvas.getByRole("button", { name: "Discard" })).toBeVisible();
  },
};

export const AttachFailure: Story = {
  args: { scenario: "attach-failure" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByTestId("terminal-attach-error"),
    ).toBeVisible();
    await expect(canvas.getByRole("button", { name: "Retry" })).toBeVisible();
  },
};

export const ReadFailure: Story = {
  args: { scenario: "read-failure" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      await canvas.findByTestId("terminal-read-error"),
    ).toBeVisible();
    await expect(canvas.getByRole("button", { name: "Retry" })).toBeVisible();
  },
};
