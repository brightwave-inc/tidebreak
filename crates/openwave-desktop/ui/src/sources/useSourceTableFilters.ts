import { useMemo, useState } from "react";

import type { LibraryDocument } from "@/documents";
import { useFacet } from "@/lib/facets";
import { documentTitle, mediaTypeLabel, statusLabel } from "./sourceFormat";

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

function mediaTypeLabelOf(document: LibraryDocument): string {
  return mediaTypeLabel(document.mediaType);
}
