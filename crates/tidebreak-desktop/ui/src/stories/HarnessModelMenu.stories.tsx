import type { Meta, StoryObj } from "@storybook/react-vite";

import { HarnessModelMenu } from "@/code/CodeComposer";
import type { CodeModelOption } from "@/code/labels";

/**
 * The code composer's model picker, and the same control as a form row in the
 * New workspace dialog.
 *
 * A vendor rail on the left narrows the list; a search box on top crosses
 * every vendor at once. Which rail entry opens depends on the catalog. An
 * engine confined to one vendor has nothing to narrow, so it opens on its only
 * block. A vendor-neutral engine opens on All: its catalog spans vendors, and
 * opening on the current model's block showed one row next to a rail of
 * unlabelled marks, which reads as a model that cannot be changed.
 */
const meta = {
  title: "Composer/Model",
  component: HarnessModelMenu,
  decorators: [
    (Story) => (
      <div className="flex items-center gap-3 p-10">
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof HarnessModelMenu>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Claude Code takes Claude models and nothing else. */
const CLAUDE_CODE: CodeModelOption[] = [
  {
    id: "claude-opus-5",
    label: "Opus 5",
    source: "Claude Code",
    vendor: "anthropic",
    default: true,
  },
  {
    id: "claude-sonnet-5",
    label: "Sonnet 5",
    source: "Claude Code",
    vendor: "anthropic",
  },
  {
    id: "claude-haiku-4-5",
    label: "Haiku 4.5",
    source: "Claude Code",
    vendor: "anthropic",
  },
];

/** opencode drives whatever its providers are signed in to. */
const OPENCODE: CodeModelOption[] = [
  {
    id: "anthropic/claude-opus-5",
    label: "Opus 5",
    source: "opencode",
    vendor: "anthropic",
  },
  {
    id: "anthropic/claude-sonnet-5",
    label: "Sonnet 5",
    source: "opencode",
    vendor: "anthropic",
  },
  {
    id: "openai/gpt-5.6-sol",
    label: "GPT 5.6 Sol",
    source: "opencode",
    vendor: "openai",
    default: true,
  },
  {
    id: "xai/grok-4.5",
    label: "Grok 4.5",
    source: "opencode",
    vendor: "xai",
  },
  {
    id: "gemini/gemini-3-pro",
    label: "Gemini 3 Pro",
    source: "opencode",
    vendor: "gemini",
  },
];

/** One vendor: no All entry, because there is nothing to lift. */
export const SingleVendor: Story = {
  args: {
    harness: "claude_code",
    options: CLAUDE_CODE,
    value: "claude-opus-5",
    onChange: () => undefined,
  },
};

/** A mixed catalog: every row is in the list, and the rail narrows it. */
export const MixedCatalog: Story = {
  args: {
    harness: "opencode",
    options: OPENCODE,
    value: "openai/gpt-5.6-sol",
    onChange: () => undefined,
  },
};

/** The form-row shape the New workspace dialog uses. */
export const FormField: Story = {
  args: {
    harness: "opencode",
    options: OPENCODE,
    value: "openai/gpt-5.6-sol",
    variant: "field",
    onChange: () => undefined,
  },
};

/** The engine is being asked what it can drive. */
export const Loading: Story = {
  args: {
    harness: "opencode",
    options: [],
    loading: true,
    variant: "field",
    onChange: () => undefined,
  },
};

/**
 * No handler: this engine takes its model when the session starts and cannot
 * be moved off it per turn. The trigger says so on hover rather than
 * disappearing.
 */
export const SetAtSessionStart: Story = {
  args: {
    harness: "opencode",
    options: OPENCODE,
    value: "openai/gpt-5.6-sol",
  },
};
