import type { Meta, StoryObj } from "@storybook/react-vite";
import { useLayoutEffect, useMemo } from "react";

import { CodingHarnessesPanel } from "@/settings/CodingHarnessesPanel";
import { useCodeUpdatesStore } from "@/code/CodeUpdatesStore";
import type { HarnessDoctorReport, HarnessUpdateChannel } from "@/api";
import {
  harnessDoctor,
  harnessDoctorCold,
  harnessDoctorDegraded,
  harnessDoctorMixed,
  harnessDoctorUpdates,
  harnessInstallsInFlight,
} from "./fixtures";
import {
  SettingsStoryHarness,
  type SettingsStoryState,
  storySettings,
} from "./SettingsStoryHarness";

/**
 * Settings → Coding harnesses, the whole page.
 *
 * The engines lead, because a reader who opens this page opens it to get one
 * working. Each row states whether it is usable and carries the one control
 * that changes that; the workspace folder follows, since it only matters once
 * an engine runs.
 */

type Variant = {
  report?: HarnessDoctorReport;
  installs?: typeof harnessInstallsInFlight;
  state?: SettingsStoryState;
  /** The stored update channel. Defaults to the pinned default. */
  channel?: HarnessUpdateChannel;
  /** What Check for updates answers with. Defaults to `report`. */
  checked?: HarnessDoctorReport;
};

function HarnessesShowcase({
  report,
  installs,
  state = "configured",
  channel = "pinned",
  checked,
}: Variant) {
  useLayoutEffect(() => {
    useCodeUpdatesStore.setState({ harnessInstalls: installs ?? {} });
  }, [installs]);
  const settings = useMemo(
    () => ({ ...storySettings, harness_update_channel: channel }),
    [channel],
  );
  const resting = report ?? harnessDoctor;
  return (
    <SettingsStoryHarness state={state} settings={settings}>
      {(client) => (
        <CodingHarnessesPanel
          client={Object.assign(client, {
            getHarnessDoctor: async () => resting,
            refreshHarnessDoctor: async () => resting,
            checkHarnessUpdates: async () => checked ?? resting,
          })}
        />
      )}
    </SettingsStoryHarness>
  );
}

const meta = {
  title: "Settings/Coding harnesses",
  component: HarnessesShowcase,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof HarnessesShowcase>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Every engine downloaded and signed in. */
export const AllReady: Story = { args: { report: harnessDoctor } };

/** A fresh machine, before anything has been downloaded. */
export const NothingDownloaded: Story = { args: { report: harnessDoctorCold } };

/** One engine in use, the rest never fetched. */
export const Mixed: Story = { args: { report: harnessDoctorMixed } };

/** A download running, and one that failed with the registry error shown. */
export const Downloading: Story = {
  args: { report: harnessDoctorCold, installs: harnessInstallsInFlight },
};

/** States no download fixes: one unverified engine and one signed-out engine. */
export const NeedsYou: Story = { args: { report: harnessDoctorDegraded } };

/**
 * The `latest` channel before anyone has asked the registry: the rows are
 * the pins, and Check for updates is the only new control.
 */
export const LatestChannel: Story = {
  args: {
    report: { ...harnessDoctor, update_channel: "latest" },
    channel: "latest",
    checked: harnessDoctorUpdates,
  },
};

/**
 * After Check for updates: one engine behind the registry with Update on
 * its row, one already moved past its pin, and the newest release in each
 * row's details.
 */
export const UpdateAvailable: Story = {
  args: { report: harnessDoctorUpdates, channel: "latest" },
};

/** The first read has not landed. */
export const Loading: Story = { args: { state: "loading" } };
