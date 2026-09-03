import type { Meta, StoryObj } from "@storybook/react-vite";
import { useMemo, useState } from "react";

import type { Facet } from "@/lib/facets";
import { FacetFilter } from "@/sources/FacetFilter";

const counts = {
  PDF: 18,
  Spreadsheet: 12,
  "Word document": 9,
  Presentation: 7,
  Markdown: 5,
  JSON: 4,
  Image: 3,
  "Plain text": 2,
};

function FacetFilterStory({
  initialSelected = [],
}: {
  initialSelected?: string[];
}) {
  const [selected, setSelected] = useState(new Set(initialSelected));
  const [search, setSearch] = useState("");
  const facet = useMemo<Facet>(
    () => ({
      selected,
      setSelected,
      search,
      setSearch,
      counts,
      toggle(value) {
        setSelected((current) => {
          const next = new Set(current);
          if (next.has(value)) next.delete(value);
          else next.add(value);
          return next;
        });
      },
    }),
    [search, selected],
  );

  return (
    <div className="flex min-h-64 items-start justify-center rounded-lg border bg-page-background p-10">
      <FacetFilter label="Type" facet={facet} />
    </div>
  );
}

const meta = {
  title: "Sources/Facet filter",
  component: FacetFilterStory,
} satisfies Meta<typeof FacetFilterStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

export const SelectedValues: Story = {
  args: { initialSelected: ["PDF", "Spreadsheet", "Presentation"] },
};

export const SearchNoMatches: Story = {};

export const Compact: Story = {
  globals: { viewport: { value: "compact", isRotated: false } },
};
