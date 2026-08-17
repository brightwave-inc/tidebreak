import workbookWasmUrl from "@dukelib/sheets-wasm/duke_sheets_wasm_bg.wasm?url";

export type EvaluatedFormulaValue =
  | { type: "boolean"; value: boolean }
  | { type: "error"; value: string }
  | { type: "number"; value: number }
  | { type: "text"; value: string };

export type EvaluatedFormulaMap = Record<string, EvaluatedFormulaValue>;

type DukeModule = typeof import("@dukelib/sheets-wasm");

let dukeModule: Promise<DukeModule> | null = null;

function loadDuke(): Promise<DukeModule> {
  dukeModule ??= import("@dukelib/sheets-wasm").then(async (mod) => {
    await mod.default({ module_or_path: workbookWasmUrl });
    return mod;
  });
  return dukeModule;
}

/**
 * Evaluate formula cells that have no authored cached result.
 *
 * The read-only viewer paints cached `<v>` values and may skip its own
 * calculation (too many formulas, or a dangling sheet reference anywhere in
 * the book). Agent-built workbooks often ship live formulas and no cache, so
 * the display copy has to compute those results itself.
 */
export async function evaluateUncachedFormulasWithDuke(
  source: ArrayBuffer,
  cells: ReadonlyArray<{ address: string; sheetIndex: number }>,
): Promise<EvaluatedFormulaMap | null> {
  if (cells.length === 0) return null;

  const { Workbook } = await loadDuke();
  const workbook = Workbook.fromBytes(new Uint8Array(source));
  try {
    workbook.calculate();
    const values: EvaluatedFormulaMap = {};
    for (const cell of cells) {
      const parsed = parseA1(cell.address);
      if (!parsed) continue;
      try {
        const evaluated = readEvaluatedValue(
          workbook.getSheet(cell.sheetIndex).getCalculatedValueAt(
            parsed.row,
            parsed.col,
          ),
        );
        if (evaluated) values[formulaValueKey(cell.sheetIndex, cell.address)] = evaluated;
      } catch {
        // A missing or unreadable sheet must not abort the rest of the bake.
      }
    }
    return Object.keys(values).length > 0 ? values : null;
  } finally {
    workbook.free();
  }
}

export function formulaValueKey(sheetIndex: number, address: string): string {
  return `${sheetIndex}:${address}`;
}

function readEvaluatedValue(value: {
  asBoolean: () => boolean | undefined;
  asError: () => string | undefined;
  asNumber: () => number | undefined;
  asText: () => string | undefined;
  is_boolean: boolean;
  is_empty: boolean;
  is_error: boolean;
  is_number: boolean;
  is_text: boolean;
}): EvaluatedFormulaValue | null {
  if (value.is_empty) return null;
  if (value.is_number) {
    const number = value.asNumber();
    return number === undefined || !Number.isFinite(number)
      ? null
      : { type: "number", value: number };
  }
  if (value.is_boolean) {
    return { type: "boolean", value: Boolean(value.asBoolean()) };
  }
  if (value.is_error) {
    const error = value.asError()?.trim();
    return error ? { type: "error", value: error } : null;
  }
  if (value.is_text) {
    const text = value.asText();
    return text === undefined ? null : { type: "text", value: text };
  }
  return null;
}

function parseA1(address: string): { col: number; row: number } | null {
  const match = address.replaceAll("$", "").match(/^([A-Z]+)(\d+)$/i);
  if (!match?.[1] || !match[2]) return null;
  let col = 0;
  for (const letter of match[1].toUpperCase()) {
    col = col * 26 + letter.charCodeAt(0) - 64;
  }
  const row = Number(match[2]) - 1;
  return row >= 0 ? { col: col - 1, row } : null;
}
