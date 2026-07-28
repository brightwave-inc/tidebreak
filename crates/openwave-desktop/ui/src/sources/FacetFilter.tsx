import { PlusCircle } from "lucide-react";

import { SearchInput } from "@/components/SearchInput";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import type { Facet } from "./useSourceTableFilters";

/**
 * One faceted filter: a dashed button that counts its selection, opening onto
 * the facet's values ordered by how many sources carry each.
 *
 * The values are searchable because a conversation can accumulate more formats
 * than fit a popover, and ordering by count rather than alphabetically puts the
 * ones worth filtering on at the top.
 */
export function FacetFilter({ label, facet }: { label: string; facet: Facet }) {
  const search = facet.search.trim().toLocaleLowerCase();
  const values = Object.entries(facet.counts)
    .filter(([value]) => value.toLocaleLowerCase().includes(search))
    .sort(([leftValue, left], [rightValue, right]) =>
      right - left || leftValue.localeCompare(rightValue),
    );

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="outline" className="border-dashed">
          <PlusCircle className="size-4" />
          {label}
          {facet.selected.size > 0 && (
            <Badge variant="secondary" size="sm">
              {facet.selected.size}
            </Badge>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="w-60 bg-background text-foreground">
        <div className="space-y-4">
          <SearchInput
            size="sm"
            placeholder={`Search ${label.toLocaleLowerCase()}…`}
            value={facet.search}
            onValueChange={facet.setSearch}
          />
          <div className="max-h-64 space-y-2 overflow-y-auto">
            {values.length === 0 ? (
              <p className="text-sm text-muted-foreground">No matches.</p>
            ) : (
              values.map(([value, count]) => (
                <label key={value} className="flex cursor-pointer items-center gap-2">
                  <Checkbox
                    checked={facet.selected.has(value)}
                    onCheckedChange={() => facet.toggle(value)}
                  />
                  <span className="flex-1 text-sm">{value}</span>
                  <span className="text-xs text-muted-foreground">{count}</span>
                </label>
              ))
            )}
          </div>
        </div>
      </PopoverContent>
    </Popover>
  );
}
