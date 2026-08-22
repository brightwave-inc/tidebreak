import { useEffect, type ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import type { ComposerWorkspaceFiles } from "@/Composer";
import { useCodeUiStore } from "@/code/CodeUiStore";
import { FORK_FRAMING, forkTranscriptFile } from "@/code/fork";
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
  workspaceFiles,
}: {
  harnesses: typeof harnessDoctor.harnesses;
  starting?: boolean;
  workspaceFiles?: ComposerWorkspaceFiles;
}) {
  // A fork seeds the framing line the same way the workspace page does, so
  // the story shows the state the reader actually lands in.
  useEffect(() => {
    if (!workspaceFiles) return;
    useCodeUiStore.getState().offerComposerPrompt("ws-1", FORK_FRAMING);
  }, [workspaceFiles]);
  return (
    <StartSessionPrompt
      workspaceId="ws-1"
      harnesses={harnesses}
      starting={starting}
      selectedMode={null}
      onSelectMode={fn()}
      onStart={fn()}
      workspaceFiles={workspaceFiles}
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

/**
 * A fork: the parent's transcript is already in the worktree, so the child
 * gets a path rather than an upload, and the framing line stays editable.
 * Any engine can be the one that reads it.
 */
export const ForkedFromAnotherAgent: Story = {
  args: {
    workspaceFiles: {
      items: [
        forkTranscriptFile({
          path: "/private/forks/9c1f4a2e.md",
          byte_len: 48_120,
          turns: 14,
          truncated: false,
        }),
      ],
      onRemove: fn(),
    },
  },
};

/** A long parent: the oldest turns did not fit, and the chip says so. */
export const ForkedFromALongSession: Story = {
  args: {
    workspaceFiles: {
      items: [
        forkTranscriptFile({
          path: "/private/forks/3b70de55.md",
          byte_len: 524_288,
          turns: 6,
          truncated: true,
        }),
      ],
      onRemove: fn(),
    },
  },
};
