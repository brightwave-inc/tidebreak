import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, fn, within } from "storybook/test";

import { ComputerUsePermissionNotice } from "@/ComputerUsePermissionNotice";
import type { PermissionRequired } from "@/computerUsePermissionAsk";
import { FloatingNotices } from "@/FloatingNotices";

const need: PermissionRequired = {
  taskId: "0d9e1c55-6d3a-4f2f-9d0e-3f1c2a7b8e41",
  permission: "screen_recording",
  browser: false,
  afterConsent: false,
};

type Notice = { need: PermissionRequired; taskName?: string | null };

/**
 * The notices tasks leave when they stopped for a missing macOS permission,
 * one per task, in the corner where they float over the work.
 */
function PermissionNoticeStory({
  need,
  taskName = null,
  others = [],
}: {
  need: PermissionRequired;
  taskName?: string | null;
  /** Notices from other tasks, stacked above this one. */
  others?: Notice[];
}) {
  const [dismissed, setDismissed] = useState<string[]>([]);
  const notices = [...others, { need, taskName }].filter(
    (notice) => !dismissed.includes(notice.need.taskId),
  );
  return (
    <div className="h-screen bg-page-background p-8">
      <div className="mx-auto max-w-2xl rounded-xl border border-border-subtle bg-background p-8">
        <p className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          Work
        </p>
        <h1 className="mt-2 text-2xl font-semibold tracking-tight">
          Tidy the Notes sidebar
        </h1>
        <p className="mt-2 max-w-lg text-sm text-muted-foreground">
          The task stopped at its first screenshot. The notice says what is
          missing without covering the conversation.
        </p>
      </div>
      <FloatingNotices>
        {notices.map((notice) => (
          <ComputerUsePermissionNotice
            key={notice.need.taskId}
            need={notice.need}
            taskName={notice.taskName}
            onAllow={fn()}
            onOpenSettings={fn()}
            onDismiss={() =>
              setDismissed((current) => [...current, notice.need.taskId])
            }
          />
        ))}
      </FloatingNotices>
    </div>
  );
}

const meta = {
  title: "Modes/Computer use permission notice",
  component: PermissionNoticeStory,
  parameters: { layout: "fullscreen" },
  args: { need },
} satisfies Meta<typeof PermissionNoticeStory>;
export default meta;
type Story = StoryObj<typeof meta>;

/** A task could not take a screenshot of another app. */
export const ScreenRecording: Story = {
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.getByRole("complementary", {
        name: "Computer use needs Screen Recording",
      }),
    ).toBeVisible();
    await expect(canvas.getByRole("button", { name: "Allow…" })).toBeEnabled();
  },
};

/** A task could not click in a browser's own window. */
export const BrowserAccessibility: Story = {
  args: { need: { ...need, permission: "accessibility", browser: true } },
  play: async ({ canvasElement }) => {
    await expect(
      within(canvasElement).getByRole("complementary", {
        name: "Browser control needs Accessibility",
      }),
    ).toBeVisible();
  },
};

/** An older helper did not say which permission was missing. */
export const UnnamedPermission: Story = {
  args: { need: { ...need, permission: null } },
};

/** The window knows the task's title, so the notice names it. */
export const NamedTask: Story = {
  args: { taskName: "Tidy the Notes sidebar" },
  play: async ({ canvasElement }) => {
    await expect(
      within(canvasElement).getByText(/“Tidy the Notes sidebar” stopped/),
    ).toBeVisible();
  },
};

/** Two tasks stopped: each leaves its own notice, named for it. */
export const TwoTasks: Story = {
  args: {
    taskName: "Tidy the Notes sidebar",
    others: [
      {
        need: {
          ...need,
          taskId: "7b0c7f1e-51a4-4f0e-bb4c-8f2d9a1c3e55",
          permission: "accessibility",
          browser: true,
        },
        taskName: "Check the release dashboard",
      },
    ],
  },
  play: async ({ canvasElement }) => {
    await expect(
      within(canvasElement).getAllByRole("complementary"),
    ).toHaveLength(2);
  },
};
