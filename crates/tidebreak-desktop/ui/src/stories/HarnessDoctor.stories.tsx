import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { DoctorList } from "@/code/DoctorList";
import {
  harnessDoctor,
  harnessDoctorCold,
  harnessDoctorDegraded,
  harnessDoctorHosted,
  harnessDoctorMixed,
  harnessInstallsInFlight,
} from "./fixtures";

/**
 * The harness doctor: one row per engine, leading with whether it is usable
 * and what closes the gap when it is not.
 *
 * Engines download one at a time, when someone picks one or presses Download
 * here, so "not downloaded" is a resting state with an action on it rather
 * than a fault. Version, path, capabilities, and probe output sit behind each
 * row's Details disclosure.
 */
const meta = {
  title: "Code/Harness doctor",
  component: DoctorList,
  args: {
    report: harnessDoctor,
    onRefresh: fn(),
    onInstall: fn(),
  },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-3xl p-8">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof DoctorList>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Every engine downloaded and signed in; capability levels differ honestly. */
export const AllReady: Story = {};

/** A fresh machine: nothing downloaded, every row one click from working. */
export const NothingDownloaded: Story = {
  args: { report: harnessDoctorCold },
};

/** The common middle: one engine in use, the rest never fetched. */
export const Mixed: Story = {
  args: { report: harnessDoctorMixed },
};

/** A download in flight and one that failed, side by side. */
export const Downloading: Story = {
  args: {
    report: harnessDoctorCold,
    installs: harnessInstallsInFlight,
  },
};

/** Nothing a download fixes: one unverified engine and one signed-out engine. */
export const NeedsYou: Story = {
  args: { report: harnessDoctorDegraded },
};

/**
 * A gateway-hosted machine: the relay engines are ready with no sign-in to
 * perform — turns run as the caller through the Model Gateway — and the
 * engines the relay does not cover yet say so.
 */
export const Hosted: Story = {
  args: { report: harnessDoctorHosted },
};

/** The re-probe is running. */
export const Rechecking: Story = {
  args: { refreshing: true },
};

/** A build that drives no engines at all. */
export const Empty: Story = {
  args: { report: { harnesses: [] } },
};
