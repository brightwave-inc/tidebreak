/**
 * Represents a range of merged cells in a spreadsheet.
 * Coordinates are 0-indexed (row 0 is Excel row 1, col 0 is Excel column A).
 */
export interface MergedCellRange {
  start: { row: number; col: number };
  end: { row: number; col: number };
}

/**
 * Determines if a hex color is dark using perceived brightness calculation.
 * Uses the weighted RGB formula that accounts for human perception.
 *
 * @param hexColor - Hex color string (with or without #, e.g., "FF0000" or "#FF0000")
 * @returns true if the color is dark (white text should be used), false if light (black text should be used)
 *
 * @example
 * isColorDark("000000") // true - black background, use white text
 * isColorDark("FFFFFF") // false - white background, use black text
 * isColorDark("0066CC") // true - dark blue background, use white text
 * isColorDark("FFA500") // false - orange background, use black text
 * isColorDark("228B22") // true - forest green background, use white text
 */
export function isColorDark(hexColor: string): boolean {
  // Remove # if present
  let hex = hexColor.replace("#", "");

  // Handle 3-digit hex colors (e.g., "F00" -> "FF0000")
  if (hex.length === 3) {
    hex = hex
      .split("")
      .map((char) => char + char)
      .join("");
  }

  // Validate hex color (should be 6 or 8 characters)
  if (hex.length !== 6 && hex.length !== 8) {
    console.warn(
      `[isColorDark] Invalid hex color: ${hexColor}, defaulting to light`,
    );
    return false; // Default to light (black text)
  }

  // Parse RGB values (ignore alpha channel if present)
  const r = parseInt(hex.substring(0, 2), 16);
  const g = parseInt(hex.substring(2, 4), 16);
  const b = parseInt(hex.substring(4, 6), 16);

  // Validate parsed values
  if (isNaN(r) || isNaN(g) || isNaN(b)) {
    console.warn(
      `[isColorDark] Failed to parse hex color: ${hexColor}, defaulting to light`,
    );
    return false;
  }

  // Calculate perceived brightness using weighted RGB formula
  // This accounts for human eye sensitivity (more sensitive to green, less to blue)
  // Formula: (R × 299 + G × 587 + B × 114) / 1000
  const brightness = (r * 299 + g * 587 + b * 114) / 1000;

  // If brightness < 128 (out of 255), it's dark
  return brightness < 128;
}

/**
 * Gets the appropriate text color (white or black) for a given background color.
 *
 * @param hexColor - Hex color string (with or without #)
 * @returns "#FFFFFF" for dark backgrounds, "#000000" for light backgrounds
 */
export function getContrastTextColor(hexColor: string): string {
  return isColorDark(hexColor) ? "#FFFFFF" : "#000000";
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

// Convert column number to Excel-style letter (0 -> A, 1 -> B, 25 -> Z, 26 -> AA, etc.)
export function columnToLetter(column: number): string {
  let temp: number;
  let letter = "";
  let col = column;
  while (col >= 0) {
    temp = col % 26;
    letter = String.fromCharCode(temp + 65) + letter;
    col = Math.floor(col / 26) - 1;
  }
  return letter;
}
