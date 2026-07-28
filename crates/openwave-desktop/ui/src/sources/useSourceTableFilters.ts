import { useMemo, useState } from "react";

import type { LibraryDocument } from "@/documents";
import { documentTitle, mediaTypeLabel, statusLabel } from "./sourceFormat";

/** One faceted filter: what is selected, what it is searched by, and the counts. */
export type Facet = {
  selected: Set<string>;
  setSelected: React.Dispatch<React.SetStateAction<Set<string>>>;
  search: string;
  setSearch: (search: string) => void;
  /** Every value in the unfiltered catalog, with how many sources carry it. */
  counts: Record<string, number>;
  toggle: (value: string) => void;
};

/**
 * Search and faceting over a conversation's sources.
 *
 * Counts come from the whole catalog rather than the filtered result, so
 * opening a facet shows the reader what selecting a value would find instead of
 * only what the current selection already allows. Sorting is the grid's job —
 * this hook decides which rows exist, not what order they sit in.
 */
export function useSourceTableFilters(documents: readonly LibraryDocument[]) {
  const [searchQuery, setSearchQuery] = useState("");
  const types = useFacet(documents, mediaTypeLabelOf);
  const statuses = useFacet(documents, statusLabel);

  const filteredDocuments = useMemo(() => {
    let result = [...documents];

    const query = searchQuery.trim().toLocaleLowerCase();
    if (query) {
      result = result.filter((document) =>
        [documentTitle(document), mediaTypeLabelOf(document), document.mediaType]
          .join(" ")
          .toLocaleLowerCase()
          .includes(query),
      );
    }

    if (types.selected.size > 0) {
      result = result.filter((document) => types.selected.has(mediaTypeLabelOf(document)));
    }
    if (statuses.selected.size > 0) {
      result = result.filter((document) => statuses.selected.has(statusLabel(document)));
    }

    return result;
  }, [documents, searchQuery, types.selected, statuses.selected]);

  const hasActiveFilters =
    searchQuery.length > 0 || types.selected.size > 0 || statuses.selected.size > 0;

  function clearAllFilters() {
    setSearchQuery("");
    types.setSelected(new Set());
    statuses.setSelected(new Set());
  }

  return {
    searchQuery,
    setSearchQuery,
    types,
    statuses,
    filteredDocuments,
    hasActiveFilters,
    clearAllFilters,
  };
}

function useFacet(
  documents: readonly LibraryDocument[],
  valueOf: (document: LibraryDocument) => string,
): Facet {
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [search, setSearch] = useState("");

  const counts = useMemo(() => {
    const totals: Record<string, number> = {};
    for (const document of documents) {
      const value = valueOf(document);
      totals[value] = (totals[value] ?? 0) + 1;
    }
    return totals;
  }, [documents, valueOf]);

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

function mediaTypeLabelOf(document: LibraryDocument): string {
  return mediaTypeLabel(document.mediaType);
}
