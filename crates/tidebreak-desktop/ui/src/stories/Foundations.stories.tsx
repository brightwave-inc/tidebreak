import type { Meta, StoryObj } from "@storybook/react-vite";
import { ArrowRight, Trash2, X } from "lucide-react";
import { expect, userEvent, within } from "storybook/test";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";

function Foundations() {
  return (
    <div className="grid max-w-3xl gap-8">
      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Actions</h2>
          <p className="text-sm text-muted-foreground">
            The common hierarchy and destructive treatment used across
            Tidebreak.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Button>Continue</Button>
          <Button variant="secondary">Save for later</Button>
          <Button variant="outline">Review details</Button>
          <Button variant="ghost">
            Open output <ArrowRight aria-hidden="true" />
          </Button>
          <Button variant="destructive">
            <Trash2 aria-hidden="true" /> Delete
          </Button>
          <Button variant="ghost-destructive">
            <X aria-hidden="true" /> Stop
          </Button>
        </div>
      </section>

      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Semantic status</h2>
          <p className="text-sm text-muted-foreground">
            These tones should remain legible in both themes without changing
            meaning.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="success">Passing</Badge>
          <Badge variant="warning">Stalled</Badge>
          <Badge variant="critical">Failed</Badge>
          <Badge variant="info">Waiting</Badge>
          <Badge variant="merged">Merged</Badge>
          <Badge variant="live">Working</Badge>
          <Badge variant="outline">Neutral</Badge>
          <Badge variant="destructive">Deleted</Badge>
        </div>
      </section>

      <section className="grid gap-3">
        <h2 className="text-base font-medium">Compact controls</h2>
        <div className="flex flex-wrap items-center gap-2">
          <Button size="lg">Large</Button>
          <Button>Default</Button>
          <Button size="sm">Small</Button>
          <Button size="xs">Extra small</Button>
          <Button size="2xs">Dense</Button>
        </div>
      </section>
    </div>
  );
}

const meta = {
  title: "Foundations/Controls and status",
  component: Foundations,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof Foundations>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Catalog: Story = {};

/** Confirms Button's outline-none utility beats the base focus-visible outline. */
export const FocusOutlineLayering: Story = {
  render: () => (
    <div className="flex flex-wrap items-center gap-2 p-4">
      <Button>Continue</Button>
      <button type="button">Native</button>
    </div>
  ),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    const designed = canvas.getByRole("button", { name: "Continue" });
    const native = canvas.getByRole("button", { name: "Native" });

    await userEvent.tab();
    await expect(designed).toHaveFocus();
    await expect(getComputedStyle(designed).outlineStyle).toBe("none");

    await userEvent.tab();
    await expect(native).toHaveFocus();
    await expect(getComputedStyle(native).outlineStyle).toBe("solid");
  },
};

/**
 * The destructive button with keyboard focus. It takes the standard ring like
 * every other control: a red wash could not reach 3:1 against the canvas.
 */
export const DestructiveFocus: Story = {
  render: () => (
    <div className="flex flex-wrap items-center gap-3 p-6">
      <Button variant="destructive">
        <Trash2 aria-hidden="true" /> Delete workspace
      </Button>
      <Button variant="outline">Cancel</Button>
    </div>
  ),
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.tab();
    await expect(
      canvas.getByRole("button", { name: "Delete workspace" }),
    ).toHaveFocus();
  },
};
