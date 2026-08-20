import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn } from "storybook/test";

import { WelcomeState } from "@/WelcomeState";
import { CodeRepoEmptyState } from "@/code/CodeHome";

type NullStatePreviewProps = {
  mode: "work" | "code";
  onAddRepo: () => void;
  onSelectPrompt: (prompt: string) => void;
};

function NullStatePreview({
  mode,
  onAddRepo,
  onSelectPrompt,
}: NullStatePreviewProps) {
  if (mode === "code") {
    return (
      <main className="content-container flex h-full min-h-0 w-full overflow-auto">
        <div className="mx-auto flex min-h-full w-full max-w-5xl items-center px-6 py-8">
          <CodeRepoEmptyState onAddRepo={onAddRepo} />
        </div>
      </main>
    );
  }

  return (
    <main className="content-container flex h-full min-h-0 w-full items-center justify-center overflow-auto px-[clamp(1rem,5vw,5rem)] py-10">
      <WelcomeState onSelectPrompt={onSelectPrompt} />
    </main>
  );
}

const meta = {
  title: "Modes/Null states",
  component: NullStatePreview,
  parameters: { layout: "fullscreen" },
  args: {
    mode: "work",
    onAddRepo: fn(),
    onSelectPrompt: fn(),
  },
  argTypes: {
    mode: { control: "inline-radio", options: ["work", "code"] },
  },
} satisfies Meta<typeof NullStatePreview>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Work mode before the first message, with complete editable starters. */
export const WorkMode: Story = {};

/** Code mode before any repository has been registered. */
export const CodeMode: Story = {
  args: { mode: "code" },
};

/** The work-mode stack at the narrowest supported conversation width. */
export const WorkModeCompact: Story = {
  globals: { viewport: { value: "compact", isRotated: false } },
};
