import { useMemo, useState } from "react";

/** One faceted filter: what is selected, what it is searched by, and the counts. */
export type Facet = {
  selected: Set<string>;
  setSelected: React.Dispatch<React.SetStateAction<Set<string>>>;
  search: string;
  setSearch: (search: string) => void;
  /** Every value in the unfiltered collection, with how many items carry it. */
  counts: Record<string, number>;
  toggle: (value: string) => void;
};

/**
 * A faceted filter over a collection, counting values across all of it.
 *
 * Counts deliberately come from the whole collection rather than the filtered
 * result, so opening a facet shows a reader what selecting a value *would* find
 * instead of only what the current selection already allows.
 */
export function useFacet<T>(
  items: readonly T[],
  valueOf: (item: T) => string,
): Facet {
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [search, setSearch] = useState("");

  const counts = useMemo(() => {
    const totals: Record<string, number> = {};
    for (const item of items) {
      const value = valueOf(item);
      totals[value] = (totals[value] ?? 0) + 1;
    }
    return totals;
  }, [items, valueOf]);

  function toggle(value: string) {
    setSelected((current) => {
      const next = new Set(current);
      if (next.has(value)) next.delete(value);
      else next.add(value);
      return next;
    });
  }

  return { selected, setSelected, search, setSearch, counts, toggle };
}
