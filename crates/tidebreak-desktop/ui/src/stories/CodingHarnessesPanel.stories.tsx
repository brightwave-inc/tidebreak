import type { Meta, StoryObj } from "@storybook/react-vite";
import { useLayoutEffect } from "react";

import { CodingHarnessesPanel } from "@/settings/CodingHarnessesPanel";
import { useCodeUpdatesStore } from "@/code/CodeUpdatesStore";
import type { HarnessDoctorReport } from "@/api";
import {
  harnessDoctor,
  harnessDoctorCold,
  harnessDoctorDegraded,
  harnessDoctorMixed,
  harnessInstallsInFlight,
} from "./fixtures";
import {
  SettingsStoryHarness,
  type SettingsStoryState,
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
};

function HarnessesShowcase({
  report,
  installs,
  state = "configured",
}: Variant) {
  useLayoutEffect(() => {
    useCodeUpdatesStore.setState({ harnessInstalls: installs ?? {} });
  }, [installs]);
  return (
    <SettingsStoryHarness state={state}>
      {(client) => (
        <CodingHarnessesPanel
          client={Object.assign(client, {
            getHarnessDoctor: async () => report ?? harnessDoctor,
            refreshHarnessDoctor: async () => report ?? harnessDoctor,
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

/** States no download fixes: no pin for one engine, no sign-in for the other. */
export const NeedsYou: Story = { args: { report: harnessDoctorDegraded } };

/** The first read has not landed. */
export const Loading: Story = { args: { state: "loading" } };
