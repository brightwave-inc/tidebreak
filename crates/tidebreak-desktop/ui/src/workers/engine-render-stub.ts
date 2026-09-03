/**
 * The formula worker never paints. `@univerjs/sheets` still imports a few
 * names from `@univerjs/engine-render`, and that package's real entry graph
 * pulls every hyphenation dictionary. Vite aliases this stub into worker
 * builds so those chunks stay off the worker graph.
 */
export class SpreadsheetSkeleton {}

export function hasCJKText(_text: string): boolean {
  return false;
}

export function precisionTo(value: number): number {
  return value;
}
