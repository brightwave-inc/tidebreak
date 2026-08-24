import type { Meta, StoryObj } from "@storybook/react-vite";

/**
 * Surfaces, hairlines, shadows, and the radius vocabulary. Depth is drawn
 * with borders and surface steps; shadows belong to things that float.
 */

function Surfaces() {
  return (
    <div className="grid max-w-3xl gap-10">
      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Surface steps</h2>
          <p className="text-sm text-muted-foreground">
            The canvas is page-background; panes and cards read on background,
            one step off the canvas in both themes. Muted recesses a region;
            popover floats above with the large shadow.
          </p>
        </div>
        <div className="rounded-xl border border-border bg-page-background p-6">
          <p className="mb-3 font-mono text-xs text-muted-foreground">
            page-background
          </p>
          <div className="rounded-lg border border-border bg-background p-4">
            <p className="mb-3 font-mono text-xs text-muted-foreground">
              background · border, not shadow
            </p>
            <div className="mb-4 rounded-md bg-muted p-3">
              <p className="font-mono text-xs text-muted-foreground">
                muted · recessed
              </p>
            </div>
            <div className="w-56 rounded-md border border-border bg-popover p-3 shadow-lg">
              <p className="font-mono text-xs text-muted-foreground">
                popover · shadow-lg, it floats
              </p>
            </div>
          </div>
        </div>
      </section>

      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Hairlines and shadows</h2>
          <p className="text-sm text-muted-foreground">
            border separates regions; border-subtle divides within a card. Three
            shadows exist, for menus, dialogs, and drag previews — resting cards
            take a border.
          </p>
        </div>
        <div className="grid grid-cols-3 gap-4">
          {(["shadow-sm", "shadow", "shadow-lg"] as const).map((name) => (
            <div
              key={name}
              className={`rounded-lg border border-border-subtle bg-background p-4 ${name}`}
            >
              <p className="font-mono text-xs text-muted-foreground">{name}</p>
            </div>
          ))}
        </div>
      </section>

      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Radius</h2>
          <p className="text-sm text-muted-foreground">
            Four steps and the pill. Cards stop at rounded-xl.
          </p>
        </div>
        <div className="flex items-end gap-4">
          {(
            [
              ["rounded-sm", "sm · 4"],
              ["rounded-md", "md · 6"],
              ["rounded-lg", "lg · 8"],
              ["rounded-xl", "xl · 12"],
              ["rounded-full", "pill"],
            ] as const
          ).map(([className, label]) => (
            <div key={label} className="grid justify-items-center gap-1.5">
              <div
                className={`size-14 border border-border bg-muted ${className}`}
              />
              <span className="font-mono text-2xs text-muted-foreground">
                {label}
              </span>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}

const meta = {
  title: "Foundations/Surfaces",
  component: Surfaces,
} satisfies Meta<typeof Surfaces>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Catalog: Story = {};
