import type { Meta, StoryObj } from "@storybook/react-vite";

/**
 * The color vocabulary, rendered from the live tokens so this page cannot
 * drift from styles.css. DESIGN.md carries the rules; this shows them.
 */

function Swatch({
  className,
  name,
  role,
}: {
  className: string;
  name: string;
  role: string;
}) {
  return (
    <div className="flex items-center gap-3">
      <div
        className={`size-9 shrink-0 rounded-md border border-border ${className}`}
      />
      <div className="min-w-0">
        <p className="font-mono text-xs">{name}</p>
        <p className="text-xs text-muted-foreground">{role}</p>
      </div>
    </div>
  );
}

const NEUTRALS: [string, string, string][] = [
  ["bg-page-background", "page-background", "the app canvas"],
  ["bg-background", "background", "reading surfaces: panes, cards, composer"],
  ["bg-muted", "muted", "recessed regions, hover washes"],
  ["bg-popover", "popover", "overlays: menus, dialogs"],
  ["bg-border", "border", "hairlines between regions"],
  ["bg-primary", "primary", "the one filled button"],
  ["bg-muted-foreground", "muted-foreground", "secondary text"],
  ["bg-foreground", "foreground", "primary text"],
];

const TONES = [
  ["success", "finished well"],
  ["warning", "needs a look"],
  ["critical", "failed, or needs you"],
  ["info", "waiting on something external"],
  ["merged", "settled outcome"],
  ["live", "an agent is working right now"],
] as const;

const ICONS = ["blue", "cyan", "violet", "amber", "rose", "green"] as const;

function Palette() {
  return (
    <div className="grid max-w-3xl gap-10">
      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Neutrals</h2>
          <p className="text-sm text-muted-foreground">
            Every gray carries a trace of cool blue (hue 240). The temperature
            is the identity; no pure or warm grays.
          </p>
        </div>
        <div className="grid grid-cols-2 gap-x-8 gap-y-3">
          {NEUTRALS.map(([className, name, role]) => (
            <Swatch key={name} className={className} name={name} role={role} />
          ))}
        </div>
      </section>

      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Status tones</h2>
          <p className="text-sm text-muted-foreground">
            Six tones, each a five-member quad. Code mode picks rungs through
            statusTone.ts, never by hand. Live is the one accent that is
            Tidebreak&apos;s own; ration it to things running this second.
          </p>
        </div>
        <div className="grid gap-2">
          {TONES.map(([tone, meaning]) => (
            <div key={tone} className="flex items-center gap-2">
              <span className="w-16 font-mono text-xs">{tone}</span>
              <span className={`size-4 rounded-full bg-${tone}`} />
              <span
                className={`rounded-full bg-${tone}-background px-2.5 py-0.5 text-xs text-${tone}-foreground-muted`}
              >
                Chip
              </span>
              <span className={`text-sm text-${tone}-foreground`}>
                Label text
              </span>
              <span className="ml-2 text-xs text-muted-foreground">
                {meaning}
              </span>
            </div>
          ))}
        </div>
      </section>

      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Identity inks</h2>
          <p className="text-sm text-muted-foreground">
            File types, repository swatches, and engine badges are identity, not
            state: the icon family, never a status tone.
          </p>
        </div>
        <div className="flex items-center gap-4">
          {ICONS.map((name) => (
            <div key={name} className="flex items-center gap-1.5">
              <span className={`size-3.5 rounded-full bg-icon-${name}`} />
              <span className="font-mono text-xs text-muted-foreground">
                {name}
              </span>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}

const meta = {
  title: "Foundations/Palette",
  component: Palette,
} satisfies Meta<typeof Palette>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Catalog: Story = {};
