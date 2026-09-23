import type { Meta, StoryObj } from "@storybook/react-vite";
import { fn, userEvent, within } from "storybook/test";

import type { PersonalInstructions } from "@/api/types";
import { InstructionsPanel } from "@/settings/InstructionsPanel";

/** Instructions as someone would write them: a few plain preferences. */
const written = [
  "Answer in British English.",
  "",
  "Lead with the answer, then the detail. Keep replies under 200 words unless I ask for more.",
  "",
  "When you write code, use TypeScript and pnpm, and never add a dependency without saying why.",
].join("\n");

/** Long enough to cross the point where the byte count appears. */
const nearLimit =
  `${written}\n\n${"Prefer tables for numeric comparisons. ".repeat(190)}`.slice(
    0,
    7_800,
  );

/** Past the cap: the page refuses to save until the text is shorter. */
const overLimit = `${nearLimit}${" Cite the source for every figure.".repeat(20)}`;

function client(
  stored: string,
  options: {
    loading?: boolean;
    loadFailure?: boolean;
    saveFailure?: boolean;
  } = {},
) {
  let current = stored;
  return {
    getPersonalInstructions: options.loading
      ? () => new Promise<PersonalInstructions>(() => undefined)
      : options.loadFailure
        ? async () => {
            throw new Error("Your instructions could not be loaded.");
          }
        : async () => ({ instructions: current }),
    putPersonalInstructions: fn(async (instructions: string) => {
      if (options.saveFailure) {
        throw new Error("Your instructions could not be saved.");
      }
      current = instructions;
      return { instructions };
    }),
  };
}

const meta = {
  title: "Settings/Instructions",
  component: InstructionsPanel,
  parameters: { layout: "fullscreen" },
  args: { client: client(written) as never },
} satisfies Meta<typeof InstructionsPanel>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Instructions someone has written and saved. */
export const Written: Story = {};

/** Nothing written yet: the placeholder suggests what belongs here. */
export const Empty: Story = {
  args: { client: client("") as never },
};

/** Past 80% of the cap, the byte count appears under the field. */
export const NearLimit: Story = {
  args: { client: client(nearLimit) as never },
};

/** Past the cap, the count turns red and the page explains why it will not save. */
export const OverLimit: Story = {
  args: { client: client(nearLimit) as never },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const field = await canvas.findByRole("textbox", {
      name: "Personal instructions",
    });
    await userEvent.click(field);
    await userEvent.paste(overLimit.slice(nearLimit.length));
    await userEvent.tab();
  },
};

export const Loading: Story = {
  args: { client: client("", { loading: true }) as never },
};

export const LoadFailed: Story = {
  args: { client: client("", { loadFailure: true }) as never },
};

/** A save that fails keeps the text and says what went wrong. */
export const SaveFailed: Story = {
  args: { client: client(written, { saveFailure: true }) as never },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const field = await canvas.findByRole("textbox", {
      name: "Personal instructions",
    });
    await userEvent.click(field);
    await userEvent.paste("\n\nUse metric units.");
    await userEvent.tab();
    await canvas.findByRole("alert");
  },
};

export const Narrow: Story = {
  decorators: [
    (Story) => (
      <div className="h-screen w-[390px] border-r border-border">
        <Story />
      </div>
    ),
  ],
};
