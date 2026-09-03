import type { Meta, StoryObj } from "@storybook/react-vite";
import { RefreshCw } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";

/**
 * Indeterminate progress. The arc is centred on the viewBox so it rotates
 * in place at every size, including the compact mark on an agent tab.
 */
const meta = {
  title: "Foundations/Spinner",
  component: Spinner,
} satisfies Meta<typeof Spinner>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

export const Sizes: Story = {
  render: () => (
    <div className="flex items-center gap-4">
      <Spinner className="size-3" />
      <Spinner className="size-3.5" />
      <Spinner className="size-4" />
      <Spinner className="size-6" />
    </div>
  ),
};

/** A busy refresh control swaps to Spinner. Do not spin RefreshCw. */
export const OnRefreshButton: Story = {
  render: () => (
    <div className="flex items-center gap-3">
      <Button type="button" size="sm" variant="outline">
        <RefreshCw />
        Refresh
      </Button>
      <Button type="button" size="sm" variant="outline" disabled>
        <Spinner aria-hidden />
        Refresh
      </Button>
    </div>
  ),
};

/**
 * The same glyph at 90° steps, pinned to a crosshair. The gap should orbit
 * the intersection, not drag the whole C around it.
 */
export const RotatesOnCenter: Story = {
  render: () => (
    <div className="flex items-center gap-6">
      {[0, 90, 180, 270].map((deg) => (
        <div key={deg} className="grid size-16 place-items-center">
          <div className="relative size-8">
            <span className="absolute inset-x-0 top-1/2 h-px -translate-y-px bg-critical/40" />
            <span className="absolute inset-y-0 left-1/2 w-px -translate-x-px bg-critical/40" />
            <Spinner
              className="size-8 animate-none text-live"
              style={{ transform: `rotate(${deg}deg)` }}
              aria-label={`${deg} degrees`}
            />
          </div>
        </div>
      ))}
    </div>
  ),
};
