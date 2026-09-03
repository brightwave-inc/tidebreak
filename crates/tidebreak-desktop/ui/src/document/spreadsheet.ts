/**
 * Represents a range of merged cells in a spreadsheet.
 * Coordinates are 0-indexed (row 0 is Excel row 1, col 0 is Excel column A).
 */
export interface MergedCellRange {
  start: { row: number; col: number };
  end: { row: number; col: number };
}

/** Parse an Excel cell address (e.g. "A1", "B5", "AA10") into 0-based row/col indices. */
export function parseCellAddress(
  address: string,
): { row: number; col: number } | null {
  const match = address.match(/^([A-Z]+)(\d+)$/);
  if (!match?.[1] || !match[2]) return null;

  const colLetters = match[1];
  const rowNum = parseInt(match[2], 10);

  let col = 0;
  for (let i = 0; i < colLetters.length; i++) {
    col = col * 26 + (colLetters.charCodeAt(i) - 65 + 1);
  }
  col -= 1;

  return { row: rowNum - 1, col };
}
