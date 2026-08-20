import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { DoctorList } from "@/code/DoctorList";
import { harnessDoctor, harnessDoctorDegraded } from "./fixtures";

/**
 * The harness doctor: every engine's probe, capability matrix, and
 * remediation. Honest capability differences (Codex's supported steering,
 * Grok's refusals) must read directly from the matrix.
 */
const meta = {
  title: "Code/Harness doctor",
  component: DoctorList,
  args: { report: harnessDoctor, onRefresh: fn() },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-2xl pt-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof DoctorList>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Every engine present and usable; capability levels differ honestly. */
export const AllReady: Story = {};

/** Missing binary and signed-out engine, each with its remediation. */
export const NeedsSetup: Story = {
  args: { report: harnessDoctorDegraded },
};

export const Refreshing: Story = {
  args: { refreshing: true },
};

/** A machine with no engines at all. */
export const Empty: Story = {
  args: { report: { harnesses: [] } },
};
