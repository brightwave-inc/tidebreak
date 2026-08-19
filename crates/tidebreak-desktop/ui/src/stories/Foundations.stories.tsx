import type { Meta, StoryObj } from "@storybook/react-vite";
import { ArrowRight, Trash2 } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";

function Foundations() {
  return (
    <div className="grid max-w-3xl gap-8">
      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Actions</h2>
          <p className="text-sm text-muted-foreground">
            The common hierarchy and destructive treatment used across Tidebreak.
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
        </div>
      </section>

      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Semantic status</h2>
          <p className="text-sm text-muted-foreground">
            These tones should remain legible in both themes without changing meaning.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Badge variant="success">Passing</Badge>
          <Badge variant="warning">Waiting</Badge>
          <Badge variant="critical">Failed</Badge>
          <Badge variant="info">Running</Badge>
          <Badge variant="outline">Neutral</Badge>
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
