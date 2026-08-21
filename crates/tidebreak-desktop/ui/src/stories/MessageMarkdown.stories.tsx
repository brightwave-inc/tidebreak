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
