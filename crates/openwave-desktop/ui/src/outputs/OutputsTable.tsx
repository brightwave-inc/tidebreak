import {
  AllCommunityModule,
  ModuleRegistry,
  type ColDef,
} from "ag-grid-community";
import { AgGridReact } from "ag-grid-react";
import { ListRestart } from "lucide-react";
import { useEffect, useMemo, useRef } from "react";

import { SearchInput } from "@/components/SearchInput";
import { Button } from "@/components/ui/button";
import type { DeliverableSummary } from "@/deliverables";
import { useFacet } from "@/lib/facets";
import { FacetFilter } from "@/sources/FacetFilter";
import { useAgGridTheme } from "@/sources/useAgGridTheme";
import {
  ActionsCellRenderer,
  DateCellRenderer,
  NameCellRenderer,
  RevisionsCellRenderer,
  SizeCellRenderer,
  TypeCellRenderer,
  type OutputGridContext,
} from "./outputCellRenderers";
import { outputTypeLabel } from "./outputFormat";
import { useState } from "react";

ModuleRegistry.registerModules([AllCommunityModule]);

type Props = {
  outputs: readonly DeliverableSummary[];
  busyOutputId: string | null;
  onOpen: (outputId: string) => void;
  onSave: (output: DeliverableSummary) => void;
  onDelete: (output: DeliverableSummary) => void;
  /** Publishes "12" or "showing 3 of 12" for the panel header to draw. */
  onCountChange: (suffix: string) => void;
};

/** The outputs catalog: search and a type facet above a virtualised grid. */
export function OutputsTable({
  outputs,
  busyOutputId,
  onOpen,
  onSave,
  onDelete,
  onCountChange,
}: Props) {
  const [searchQuery, setSearchQuery] = useState("");
  const types = useFacet(outputs, typeOf);
  const gridTheme = useAgGridTheme();
  const gridRef = useRef<AgGridReact<DeliverableSummary>>(null);

  const filteredOutputs = useMemo(() => {
    let result = [...outputs];
    const query = searchQuery.trim().toLocaleLowerCase();
    if (query) {
      result = result.filter((output) =>
        [output.filename, outputTypeLabel(output.mediaType)]
          .join(" ")
          .toLocaleLowerCase()
          .includes(query),
      );
    }
    if (types.selected.size > 0) {
      result = result.filter((output) => types.selected.has(typeOf(output)));
    }
    return result;
  }, [outputs, searchQuery, types.selected]);

  const hasActiveFilters = searchQuery.length > 0 || types.selected.size > 0;

  const gridContext = useMemo<OutputGridContext>(
    () => ({ onOpen, onSave, onDelete, busyOutputId }),
    [onOpen, onSave, onDelete, busyOutputId],
  );

  // Cell renderers read their actions off `context`, which the grid snapshots
  // rather than re-reading, so a changed callback needs the cells redrawn.
  useEffect(() => {
    gridRef.current?.api?.refreshCells({ force: true });
  }, [gridContext]);

  const columnDefs = useMemo<ColDef<DeliverableSummary>[]>(
    () => [
      {
        headerName: "Name",
        field: "filename",
        flex: 1,
        minWidth: 220,
        cellRenderer: NameCellRenderer,
        sortable: true,
        comparator: (left: string, right: string) =>
          left.localeCompare(right, undefined, { sensitivity: "base" }),
      },
      {
        headerName: "Type",
        field: "mediaType",
        width: 120,
        cellRenderer: TypeCellRenderer,
        sortable: true,
        comparator: (_l, _r, leftNode, rightNode) =>
          outputTypeLabel(leftNode.data!.mediaType).localeCompare(
            outputTypeLabel(rightNode.data!.mediaType),
          ),
      },
      {
        headerName: "Size",
        field: "sizeBytes",
        width: 100,
        cellRenderer: SizeCellRenderer,
        sortable: true,
      },
      {
        headerName: "Revisions",
        field: "revisionCount",
        width: 110,
        cellRenderer: RevisionsCellRenderer,
        sortable: true,
      },
      {
        headerName: "Updated",
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

  useEffect(() => {
    onCountChange(
      filteredOutputs.length !== outputs.length
        ? `showing ${filteredOutputs.length} of ${outputs.length}`
        : String(outputs.length),
    );
  }, [filteredOutputs.length, outputs.length, onCountChange]);

  return (
    <div className="flex h-full min-h-0 flex-col gap-4 px-4 pb-4">
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-3">
        <SearchInput
          placeholder="Search outputs…"
          value={searchQuery}
          onValueChange={setSearchQuery}
          className="max-w-md min-w-64 flex-1"
        />
        <div className="flex flex-shrink-0 flex-wrap items-center gap-2">
          {hasActiveFilters && (
            <Button
              variant="outline"
              size="icon"
              className="border-dashed"
              title="Reset all filters"
              onClick={() => {
                setSearchQuery("");
                types.setSelected(new Set());
              }}
            >
              <ListRestart className="size-4" />
              <span className="sr-only">Reset all filters</span>
            </Button>
          )}
          <FacetFilter label="Type" facet={types} />
        </div>
      </div>

      <div className="relative min-h-0 flex-1">
        {filteredOutputs.length === 0 ? (
          <div className="flex h-full items-center justify-center">
            <p className="text-sm text-muted-foreground italic">
              No outputs match your search.
            </p>
          </div>
        ) : (
          <div className="size-full">
            <AgGridReact<DeliverableSummary>
              ref={gridRef}
              theme={gridTheme}
              context={gridContext}
              rowData={filteredOutputs as DeliverableSummary[]}
              columnDefs={columnDefs}
              defaultColDef={defaultColDef}
              suppressMovableColumns
              suppressCellFocus
              getRowId={(params) => params.data.outputId}
              domLayout="normal"
            />
          </div>
        )}
      </div>
    </div>
  );
}

function typeOf(output: DeliverableSummary): string {
  return outputTypeLabel(output.mediaType);
}
