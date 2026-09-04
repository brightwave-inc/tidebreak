import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import { BrowserNoticeRow } from "@/code/browser/BrowserToolbar";

const meta = {
  title: "Foundations/Notices",
  parameters: { layout: "padded" },
} satisfies Meta;
export default meta;
type Story = StoryObj<typeof meta>;
export const Tones: Story = {
  render: () => (
    <div className="mx-auto flex max-w-3xl flex-col gap-6">
      <div className="space-y-2">
        <h2 className="text-lg font-medium">Notices</h2>
        <p className="text-sm text-muted-foreground">
          Neutral surfaces keep the message readable. The leading edge marks its
          status.
        </p>
      </div>
      {(["info", "success", "warning", "critical"] as const).map((tone) => (
        <div
          key={tone}
          className={
            "notice-surface notice-" +
            tone +
            " rounded-md border px-3 py-2 text-sm"
          }
        >
          <p className="font-medium">
            {
              {
                info: "Session resumed",
                success: "Changes saved",
                warning: "Connection interrupted",
                critical: "Could not save changes",
              }[tone]
            }
          </p>
          <p className="mt-1 text-muted-foreground">
            {
              {
                info: "You can continue from the last message.",
                success: "Your preferences apply to the next session.",
                warning: "Check your connection before trying again.",
                critical: "Your edits are still here. Reconnect and try again.",
              }[tone]
            }
          </p>
        </div>
      ))}
      <BrowserNoticeRow
        tone="warning"
        message="This page needs permission to open an external application."
        actionLabel="Open"
        onAction={fn()}
        onDismiss={fn()}
      />
    </div>
  ),
};
