import type { Meta, StoryObj } from "@storybook/react-vite";
import type { Attention } from "@/api/types";
import { SessionAttentionBadge } from "@/code/workspace/badges";
import {
  attentionDoneUnreviewed,
  attentionFenced,
  attentionManual,
  attentionNeedsYou,
  attentionStalled,
  attentionWorking,
} from "./fixtures";

const meta = {
  title: "Code/Session attention badge",
  component: SessionAttentionBadge,
  parameters: { layout: "padded" },
} satisfies Meta<typeof SessionAttentionBadge>;

export default meta;
type Story = StoryObj<typeof meta>;

const states: Array<{ label: string; attention: Attention | undefined }> = [
  { label: "Loading", attention: undefined },
  { label: "Working", attention: attentionWorking },
  {
    label: "Idle",
    attention: { state: { type: "idle" }, source: "lifecycle" },
  },
  { label: "Needs you", attention: attentionNeedsYou },
  { label: "Stalled", attention: attentionStalled },
  { label: "Done", attention: attentionDoneUnreviewed },
  { label: "Reconnecting", attention: attentionFenced },
  { label: "Pinned", attention: attentionManual },
];

export const DigestStates: Story = {
  args: { attention: attentionNeedsYou },
  render: () => (
    <div className="flex max-w-sm flex-col divide-y divide-border text-sm">
      {states.map(({ label, attention }) => (
        <div key={label} className="flex min-h-10 items-center gap-3 py-2">
          <span className="w-24 shrink-0 text-muted-foreground">{label}</span>
          <SessionAttentionBadge attention={attention} />
        </div>
      ))}
    </div>
  ),
};
