import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { QuitPrompt } from "@/QuitPrompt";
import type { QuitPromptState } from "@/desktopLifecycle";

function QuitPromptStory({
  prompt,
  error,
  answering,
}: {
  prompt: QuitPromptState;
  error: string | null;
  answering: boolean;
}) {
  return (
    <div className="h-screen bg-page-background p-8">
      <div className="mx-auto max-w-2xl rounded-xl border border-border-subtle bg-background p-8">
        <p className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          Conversation
        </p>
        <h1 className="mt-2 text-2xl font-semibold tracking-tight">
          Plan the next release
        </h1>
        <p className="mt-2 max-w-lg text-sm text-muted-foreground">
          An agent is still working here when the quit arrives.
        </p>
      </div>
      <QuitPrompt
        prompt={prompt}
        error={error}
        answering={answering}
        onChoose={fn()}
        onOpenInbox={fn()}
      />
    </div>
  );
}

const meta = {
  title: "Shell/Quit prompt",
  component: QuitPromptStory,
  parameters: { layout: "fullscreen" },
  args: {
    prompt: { phase: "asking", agents: 2, waitingForYou: 0 },
    error: null,
    answering: false,
  },
} satisfies Meta<typeof QuitPromptStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/** The first question: how many agents are working, and three choices. */
export const Asking: Story = {};

export const OneAgent: Story = {
  args: { prompt: { phase: "asking", agents: 1, waitingForYou: 0 } },
};

/**
 * An agent parked on an approval reaches a safe point only after the person
 * answers, so the prompt says so and offers the inbox.
 */
export const WaitingForYourAnswer: Story = {
  args: { prompt: { phase: "asking", agents: 3, waitingForYou: 1 } },
};

/**
 * Quitting at a safe point: a bar counts the agents down and leaves the app
 * usable behind it.
 */
export const WaitingForASafePoint: Story = {
  args: { prompt: { phase: "waiting", agents: 2, waitingForYou: 0 } },
};

/** The wait, with an agent that needs an answer before it can finish. */
export const WaitingOnAnAnswer: Story = {
  args: { prompt: { phase: "waiting", agents: 2, waitingForYou: 1 } },
};

export const Stopping: Story = {
  args: { prompt: { phase: "stopping" } },
};

/** A wait that could not confirm a safe point asks again, with the reason. */
export const SafePointFailed: Story = {
  args: {
    prompt: { phase: "asking", agents: 1, waitingForYou: 0 },
    error: "Chat turns could not be released in time. Try again in a moment.",
  },
};

/** An answer is on its way to the shell. */
export const Answering: Story = {
  args: { answering: true },
};
