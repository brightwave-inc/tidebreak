import type { ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import { StartSessionPrompt } from "@/code/StartSessionPrompt";
import { harnessDoctor, harnessDoctorDegraded } from "./fixtures";

/**
 * The session-start surface: harness picker, caps-driven permission modes,
 * and the first message. Mode lists must follow each engine's declared
 * capabilities — Grok's honest refusals leave it Auto-only.
 */
function withRouter(children: ReactNode) {
  const rootRoute = createRootRoute({ component: () => children });
  const router = createRouter({
    routeTree: rootRoute,
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  return <RouterProvider router={router as never} />;
}

function StartSession({
  harnesses,
  starting = false,
}: {
  harnesses: typeof harnessDoctor.harnesses;
  starting?: boolean;
}) {
  return (
    <StartSessionPrompt
      workspaceId="ws-1"
      harnesses={harnesses}
      starting={starting}
      selectedMode={null}
      onSelectMode={fn()}
      onStart={fn()}
    />
  );
}

const meta = {
  title: "Code/Start session",
  component: StartSession,
  args: { harnesses: harnessDoctor.harnesses },
  decorators: [
    (Story) => (
      <div className="mx-auto max-w-xl pt-8">{withRouter(<Story />)}</div>
    ),
  ],
} satisfies Meta<typeof StartSession>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Four engines, each with its honest mode list. */
export const AllEngines: Story = {};

export const Starting: Story = {
  args: { starting: true },
};

/** Engines that need install or sign-in before a session can start. */
export const NeedsSetup: Story = {
  args: { harnesses: harnessDoctorDegraded.harnesses },
};
