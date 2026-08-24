import { useEffect, type ReactNode } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";
import {
  createMemoryHistory,
  createRootRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import type { CodeForkTranscript } from "@/api/types";
import { useCodeUiStore } from "@/code/CodeUiStore";
import { forkFraming, forkTranscriptFile } from "@/code/fork";
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
  fork,
}: {
  harnesses: typeof harnessDoctor.harnesses;
  starting?: boolean;
  fork?: CodeForkTranscript;
}) {
  // A fork seeds the framing lines the same way the workspace page does, so
  // the story shows the state the reader actually lands in.
  useEffect(() => {
    if (!fork) return;
    useCodeUiStore.getState().offerComposerPrompt("ws-1", forkFraming(fork));
  }, [fork]);
  return (
    <StartSessionPrompt
      workspaceId="ws-1"
      harnesses={harnesses}
      starting={starting}
      selectedMode={null}
      onSelectMode={fn()}
      onStart={fn()}
      workspaceFiles={
        fork ? { items: [forkTranscriptFile(fork)], onRemove: fn() } : undefined
      }
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
 * A fork: the parent's handoff is already on disk, so the child gets a path
 * rather than an upload, and the framing lines stay editable. Any engine can
 * be the one that reads it.
 */
export const ForkedFromAnotherAgent: Story = {
  args: {
    fork: {
      path: "/private/forks/9c1f4a2e/6a01/transcript.md",
      dir: "/private/forks/9c1f4a2e/6a01",
      byte_len: 48_120,
      turns: 14,
      total_turns: 14,
      truncated: false,
    },
  },
};

/**
 * A fork taken at an earlier turn: the framing warns that the worktree can
 * be ahead of the transcript.
 */
export const ForkedFromAnEarlierTurn: Story = {
  args: {
    fork: {
      path: "/private/forks/9c1f4a2e/8b22/transcript.md",
      dir: "/private/forks/9c1f4a2e/8b22",
      byte_len: 21_004,
      turns: 7,
      total_turns: 7,
      at_turn_ordinal: 7,
      truncated: false,
    },
  },
};

/** A long parent: the oldest turns did not fit in full, and the chip says so. */
export const ForkedFromALongSession: Story = {
  args: {
    fork: {
      path: "/private/forks/3b70de55/1c9d/transcript.md",
      dir: "/private/forks/3b70de55/1c9d",
      byte_len: 524_288,
      turns: 6,
      total_turns: 41,
      truncated: true,
    },
  },
};
