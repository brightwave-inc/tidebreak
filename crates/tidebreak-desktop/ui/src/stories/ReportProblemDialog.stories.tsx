import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { ReportProblemDialog } from "@/ReportProblemDialog";
import type { ProblemReportFacts } from "@/reportProblem";

const FACTS: ProblemReportFacts = {
  version: "0.117.0",
  os: "macOS 15.6",
  arch: "arm64",
};

/**
 * Help → Report a Problem…, and every other way in: the boot screen, a
 * crash, Settings → Updates, and a workspace's menu.
 *
 * It saves the diagnostics report to a file the person picks, then opens a
 * GitHub issue with the version, operating system, and architecture filled
 * in, and it shows those three before anything opens. A workspace with a
 * session adds the agent path from decision 81.
 */
const meta = {
  title: "Shell/Report a problem",
  component: ReportProblemDialog,
  parameters: { layout: "fullscreen" },
  args: {
    open: true,
    onOpenChange: fn(),
    readFacts: fn(async () => FACTS),
    saveReport: fn(async () => true),
    openUrl: fn(async () => {}),
  },
} satisfies Meta<typeof ReportProblemDialog>;

export default meta;
type Story = StoryObj<typeof meta>;

/** From the Help menu, the boot screen, a crash, or Settings. */
export const FromHelpMenu: Story = {};

/** From a workspace with a session: the agent can take the session along. */
export const FromWorkspace: Story = {
  args: { onAskAgent: fn() },
};

/** The native save dialog is open, or the report is being written. */
export const Saving: Story = {
  args: { initialPhase: { step: "saving" } },
};

/** The report is saved and the issue opened in the browser. */
export const Saved: Story = {
  args: { initialPhase: { step: "saved" } },
};

/** The report could not be built or written; the issue is still on offer. */
export const SaveFailed: Story = {
  args: {
    initialPhase: {
      step: "failed",
      message: "Could not save the diagnostics report",
    },
  },
};

/** Outside the desktop the shell cannot say what it runs on. */
export const FactsUnavailable: Story = {
  args: {
    readFacts: fn(async () => ({ version: null, os: null, arch: null })),
  },
};
