import type { Meta, StoryObj } from "@storybook/react-vite";

/**
 * The pinned type scale and the two voices. The rungs and their jobs live in
 * DESIGN.md; sizes here come from the tokens, so what renders is what ships.
 */

const SCALE: [string, string, string, string][] = [
  ["text-2xs", "2xs", "10 / 14", "micro labels, counters, avatar initials"],
  ["text-xs", "xs", "11 / 15", "dense metadata, timestamps, eyebrows"],
  ["text-sm", "sm", "12.5 / 18", "the working size for chrome"],
  ["text-md", "md", "13.5 / 20", "content: transcripts, card titles"],
  ["text-base", "base", "14 / 21", "body text"],
  ["text-lg", "lg", "16 / 24", "section headings"],
  ["text-xl", "xl", "18 / 26", "pane headings"],
  ["text-2xl", "2xl", "20 / 28", "welcome and empty states"],
  ["text-3xl", "3xl", "24 / 30", "display"],
];

function Typography() {
  return (
    <div className="grid max-w-3xl gap-10">
      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Scale</h2>
          <p className="text-sm text-muted-foreground">
            Pinned in px on a 14px root. Arbitrary sizes fail the styles
            contract test; a genuine new rung goes into the scale instead.
          </p>
        </div>
        <div className="grid gap-2">
          {SCALE.map(([className, name, size, job]) => (
            <div
              key={name}
              className="flex items-baseline gap-4 border-b border-border-subtle pb-2"
            >
              <span className="w-10 shrink-0 font-mono text-xs text-muted-foreground">
                {name}
              </span>
              <span className="w-14 shrink-0 whitespace-nowrap font-mono text-xs text-muted-foreground">
                {size}
              </span>
              <span className={`${className} truncate`}>
                Agents work while you watch
              </span>
              <span className="ml-auto hidden shrink-0 text-xs text-muted-foreground sm:block">
                {job}
              </span>
            </div>
          ))}
        </div>
      </section>

      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Weights</h2>
          <p className="text-sm text-muted-foreground">
            Hierarchy comes from size; weight stops at semibold.
          </p>
        </div>
        <div className="flex items-baseline gap-6 text-md">
          <span className="font-normal">400 text</span>
          <span className="font-medium">500 emphasis</span>
          <span className="font-semibold">600 titles</span>
        </div>
      </section>

      <section className="grid gap-3">
        <div>
          <h2 className="text-base font-medium">Two voices</h2>
          <p className="text-sm text-muted-foreground">
            Geist is how the product speaks; Geist Mono is how the machine
            speaks. Identifiers the machine produced render in mono.
          </p>
        </div>
        <div className="grid gap-2 rounded-lg border border-border bg-background p-4">
          <p className="text-md">
            Merge when the checks pass, then delete the branch.
          </p>
          <p className="font-mono text-xs text-muted-foreground">
            thet/read-refero-style-page · 5f4129cae · claude-sonnet-5 · 42.3k
            tokens
          </p>
          <p className="text-2xs font-mono uppercase tracking-wide text-muted-foreground">
            Waiting on checks
          </p>
        </div>
      </section>
    </div>
  );
}

const meta = {
  title: "Foundations/Typography",
  component: Typography,
} satisfies Meta<typeof Typography>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Catalog: Story = {};
