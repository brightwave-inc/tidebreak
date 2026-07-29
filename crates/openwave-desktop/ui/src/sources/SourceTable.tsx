import {
  AllCommunityModule,
  ModuleRegistry,
  type ColDef,
  type GridApi,
  type GridReadyEvent,
} from "ag-grid-community";
import { AgGridReact } from "ag-grid-react";
import { ListRestart } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { SearchInput } from "@/components/SearchInput";
import { Button } from "@/components/ui/button";
import type { LibraryDocument } from "@/documents";
import { FacetFilter } from "./FacetFilter";
import {
  ActionsCellRenderer,
  DateCellRenderer,
  NameCellRenderer,
  SizeCellRenderer,
  TypeCellRenderer,
  type SourceGridContext,
} from "./sourceCellRenderers";
import { documentTitle, mediaTypeLabel } from "./sourceFormat";
import { useAgGridTheme } from "./useAgGridTheme";
import { useSourceTableFilters } from "./useSourceTableFilters";

ModuleRegistry.registerModules([AllCommunityModule]);

type Props = {
  documents: readonly LibraryDocument[];
  busyDocumentId: string | null;
  canDownload: boolean;
  onOpen: (documentId: string) => void;
  onDownload: (document: LibraryDocument) => void;
  onDelete: (document: LibraryDocument) => void;
  onDeleteMany: (documents: LibraryDocument[]) => Promise<void>;
  onRetry: (document: LibraryDocument) => void;
  /** Publishes "12" or "showing 3 of 12" for the panel header to draw. */
  onCountChange: (suffix: string) => void;
};

/**
 * The source catalog: search and facets above a virtualised grid.
 *
 * The grid is virtualised because a conversation can hold the newest thousand
 * sources and every row draws a popover-capable status pill — rendering all of
 * them to scroll through a dozen is what the old table did.
 */
export function SourceTable({
  documents,
  busyDocumentId,
  canDownload,
  onOpen,
  onDownload,
  onDelete,
  onDeleteMany,
  onRetry,
  onCountChange,
}: Props) {
  const filters = useSourceTableFilters(documents);
  const { filteredDocuments } = filters;
  const gridTheme = useAgGridTheme();
  const gridRef = useRef<AgGridReact<LibraryDocument>>(null);
  const [gridApi, setGridApi] = useState<GridApi<LibraryDocument> | null>(null);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [deleting, setDeleting] = useState(false);

  const gridContext = useMemo<SourceGridContext>(
    () => ({ onOpen, onDownload, onDelete, onRetry, canDownload, busyDocumentId }),
    [onOpen, onDownload, onDelete, onRetry, canDownload, busyDocumentId],
  );

  // Cell renderers read the actions off `context`, which the grid snapshots
  // rather than re-reading, so a changed callback needs the cells redrawn.
  useEffect(() => {
    gridRef.current?.api?.refreshCells({ force: true });
  }, [gridContext]);

  const columnDefs = useMemo<ColDef<LibraryDocument>[]>(
    () => [
      {
        headerName: "Name",
        field: "title",
        flex: 1,
        minWidth: 250,
        cellRenderer: NameCellRenderer,
        sortable: true,
        comparator: (_left, _right, leftNode, rightNode) =>
          collate(documentTitle(leftNode.data!), documentTitle(rightNode.data!)),
      },
      {
        headerName: "Type",
        field: "mediaType",
        width: 120,
        cellRenderer: TypeCellRenderer,
        sortable: true,
        comparator: (_left, _right, leftNode, rightNode) =>
          collate(
            mediaTypeLabel(leftNode.data!.mediaType),
            mediaTypeLabel(rightNode.data!.mediaType),
          ),
      },
      {
        headerName: "Size",
        field: "sizeBytes",
        width: 100,
        cellRenderer: SizeCellRenderer,
        sortable: true,
        // A source of unknown size sorts last either way rather than reading as
        // the smallest file in the conversation.
        comparator: (left: number | null, right: number | null, _l, _r, descending) => {
          if (left === right) return 0;
          if (left === null) return descending ? -1 : 1;
          if (right === null) return descending ? 1 : -1;
          return left - right;
        },
      },
      {
        headerName: "Added",
        field: "updatedAt",
        width: 110,
        cellRenderer: DateCellRenderer,
        sortable: true,
        sort: "desc",
        comparator: (left: string, right: string) => Date.parse(left) - Date.parse(right),
      },
      {
        headerName: "",
        colId: "actions",
        width: 56,
        cellRenderer: ActionsCellRenderer,
        resizable: false,
        sortable: false,
      },
    ],
    [],
  );

  const defaultColDef = useMemo<ColDef>(
    () => ({ resizable: true, suppressMovable: true }),
    [],
  );

  const onGridReady = useCallback((event: GridReadyEvent<LibraryDocument>) => {
    setGridApi(event.api);
  }, []);

  const onRowSelected = useCallback(() => {
    if (!gridApi) return;
    setSelectedIds(new Set(gridApi.getSelectedRows().map((row) => row.documentId)));
  }, [gridApi]);

  // A source deleted elsewhere, or filtered out of view, must not stay in a
  // selection whose bulk actions would then act on rows the reader cannot see.
  useEffect(() => {
    setSelectedIds((current) => {
      if (current.size === 0) return current;
      const live = new Set(documents.map((document) => document.documentId));
      const next = new Set([...current].filter((id) => live.has(id)));
      return next.size === current.size ? current : next;
    });
  }, [documents]);

  const clearSelection = useCallback(() => {
    gridApi?.deselectAll();
    setSelectedIds(new Set());
  }, [gridApi]);

  const selectedDocuments = documents.filter((document) =>
    selectedIds.has(document.documentId),
  );
  const visibleSelectedCount = filteredDocuments.filter((document) =>
    selectedIds.has(document.documentId),
  ).length;
  const hiddenSelectedCount = selectedIds.size - visibleSelectedCount;

  useEffect(() => {
    onCountChange(
      filteredDocuments.length !== documents.length
        ? `showing ${filteredDocuments.length} of ${documents.length}`
        : String(documents.length),
    );
  }, [filteredDocuments.length, documents.length, onCountChange]);

  async function deleteSelected() {
    if (deleting) return;
    setDeleting(true);
    try {
      // Whatever was actually deleted leaves `documents`, and the prune effect
      // takes it out of the selection — so a cancelled confirmation correctly
      // leaves the selection standing.
      await onDeleteMany(selectedDocuments);
    } finally {
      setDeleting(false);
    }
  }

  const retriableSelected = selectedDocuments.filter(
    (document) => document.failure?.retriable,
  );

  return (
    <div className="flex h-full min-h-0 flex-col gap-4 px-4 pb-4">
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-3">
        <SearchInput
          placeholder="Search sources…"
          value={filters.searchQuery}
          onValueChange={filters.setSearchQuery}
          className="max-w-md min-w-64 flex-1"
        />
        <div className="flex flex-shrink-0 flex-wrap items-center gap-2">
          {filters.hasActiveFilters && (
            <Button
              variant="outline"
              size="icon"
              className="border-dashed"
              title="Reset all filters"
              onClick={filters.clearAllFilters}
            >
              <ListRestart className="size-4" />
              <span className="sr-only">Reset all filters</span>
            </Button>
          )}
          <FacetFilter label="Type" facet={filters.types} />
          <FacetFilter label="Status" facet={filters.statuses} />
        </div>
      </div>

      <div className="relative min-h-0 flex-1">
        {selectedIds.size > 0 && (
          <div className="absolute top-px right-px left-[51px] z-10 flex h-10 items-center justify-between bg-page-background px-4">
            <span className="text-sm font-medium">
              {selectedIds.size} selected
              {hiddenSelectedCount > 0 && (
                <span className="text-muted-foreground">
                  {" "}
                  ({visibleSelectedCount} visible, {hiddenSelectedCount} hidden)
                </span>
              )}
            </span>
            <div className="flex items-center gap-2">
              {retriableSelected.length > 0 && (
                <Button
                  variant="outline"
                  size="xs"
                  onClick={() => retriableSelected.forEach(onRetry)}
                >
                  Retry {retriableSelected.length === 1 ? "" : retriableSelected.length}
                </Button>
              )}
              <Button variant="outline" size="xs" onClick={clearSelection}>
                Clear
              </Button>
              <Button
                variant="destructive"
                size="xs"
                disabled={deleting}
                onClick={() => void deleteSelected()}
              >
                {deleting ? "Deleting…" : "Delete"}
              </Button>
            </div>
          </div>
        )}

        {filteredDocuments.length === 0 ? (
          <div className="flex h-full items-center justify-center">
            <p className="text-sm text-muted-foreground italic">
              No sources match your search.
            </p>
          </div>
        ) : (
          <div className="size-full">
            <AgGridReact<LibraryDocument>
              ref={gridRef}
              theme={gridTheme}
              context={gridContext}
              rowData={filteredDocuments as LibraryDocument[]}
              columnDefs={columnDefs}
              defaultColDef={defaultColDef}
              rowSelection={{ mode: "multiRow", enableClickSelection: false }}
              suppressMovableColumns
              suppressCellFocus
              onGridReady={onGridReady}
              onRowSelected={onRowSelected}
              getRowId={(params) => params.data.documentId}
              domLayout="normal"
            />
          </div>
        )}
      </div>
    </div>
  );
}

function collate(left: string, right: string): number {
  return left.localeCompare(right, undefined, { sensitivity: "base" });
}
