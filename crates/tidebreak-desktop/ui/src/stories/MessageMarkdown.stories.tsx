import type { Meta, StoryObj } from "@storybook/react-vite";

import { MessageMarkdown } from "@/MessageMarkdown";

const SAMPLE = `## Review summary

The focused component suite passed, and the static workshop build completed successfully.

- **Behavior:** existing Vitest coverage remains authoritative
- **Visual evaluation:** stories expose meaningful product states
- **Production:** Storybook remains a development-only dependency

| Surface | Initial coverage | Next step |
| --- | ---: | --- |
| Conversation | 4 states | Add tool-result extremes |
| Code workspace | 5 states | Add diff stress cases |

\`\`\`ts
const workshop = {
  scope: "deliberate",
  productionBundle: false,
};
\`\`\`

Inline math still renders: $x^2 + y^2 = z^2$.
`;

const meta = {
  title: "Conversation/Message markdown",
  component: MessageMarkdown,
  args: { children: SAMPLE },
  decorators: [
    (Story) => (
      <article className="mx-auto max-w-3xl rounded-xl bg-background px-6 py-5 shadow-sm">
        <Story />
      </article>
    ),
  ],
} satisfies Meta<typeof MessageMarkdown>;

export default meta;
type Story = StoryObj<typeof meta>;

export const RichResponse: Story = {};

export const NarrowResponse: Story = {
  globals: { viewport: { value: "compact", isRotated: false } },
};

export const HighlightedPassage: Story = {
  args: {
    children:
      "The workshop should stay small enough to remain trustworthy and fast.",
    highlightRange: { start: 4, end: 18 },
  },
};

/** A localhost URL must stay one link even when the line wraps at the query. */
export const WrappedLocalUrl: Story = {
  args: {
    children:
      "Storybook is still at http://127.0.0.1:6031/?path=/story/code-workspace-card--hover-idle-session if you want another look.",
  },
  decorators: [
    (Story) => (
      <article className="message message-assistant mx-auto w-80 rounded-xl bg-background px-4 py-3">
        <Story />
      </article>
    ),
  ],
};
