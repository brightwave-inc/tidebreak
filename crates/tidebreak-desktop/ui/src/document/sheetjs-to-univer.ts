import type {
  ICellData,
  IWorkbookData,
  IWorksheetData,
  IStyleData,
  IColumnData,
  IRowData,
  IRange,
} from "@univerjs/presets";
import {
  BooleanNumber,
  BorderStyleTypes,
  CellValueType,
  HorizontalAlign,
  LocaleType,
  VerticalAlign,
  WrapStrategy,
} from "@univerjs/presets";
import type { WorkBook, WorkSheet } from "xlsx";
import * as XLSX from "xlsx";

import type {
  ParsedBorderSide,
  ParsedCellStyle,
  ParsedFreezePane,
  XlsxCellStyleMap,
} from "./xlsx-styles-parser";

// ── Style mapping helpers ────────────────────────────────────────────────────

function mapHorizontalAlign(align: string): HorizontalAlign | undefined {
  switch (align) {
    case "left":
      return HorizontalAlign.LEFT;
    case "center":
    case "centerContinuous":
      return HorizontalAlign.CENTER;
    case "right":
      return HorizontalAlign.RIGHT;
    case "justify":
    case "distributed":
      return HorizontalAlign.JUSTIFIED;
    default:
      return undefined;
  }
}

function mapVerticalAlign(align: string): VerticalAlign | undefined {
  switch (align) {
    case "top":
      return VerticalAlign.TOP;
    case "center":
      return VerticalAlign.MIDDLE;
    case "bottom":
      return VerticalAlign.BOTTOM;
    default:
      return undefined;
  }
}

// Map common non-web-safe fonts to their closest web-safe equivalents so
// the canvas renders the right family even when the original font isn't
// installed (e.g. Calibri isn't available on macOS).
const FONT_SUBSTITUTIONS: Record<string, string> = {
  calibri: "Arial",
  cambria: "Georgia",
  "calibri light": "Arial",
  "cambria math": "Georgia",
  consolas: "Courier New",
  candara: "Trebuchet MS",
  constantia: "Palatino Linotype",
  corbel: "Verdana",
};

/** Resolve a font name to a web-safe equivalent when necessary */
function resolveFont(name: string): string {
  return FONT_SUBSTITUTIONS[name.toLowerCase()] ?? name;
}

const OOXML_BORDER_STYLE_MAP: Record<string, BorderStyleTypes> = {
  thin: BorderStyleTypes.THIN,
  hair: BorderStyleTypes.HAIR,
  dotted: BorderStyleTypes.DOTTED,
  dashed: BorderStyleTypes.DASHED,
  dashDot: BorderStyleTypes.DASH_DOT,
  dashDotDot: BorderStyleTypes.DASH_DOT_DOT,
  double: BorderStyleTypes.DOUBLE,
  medium: BorderStyleTypes.MEDIUM,
  mediumDashed: BorderStyleTypes.MEDIUM_DASHED,
  mediumDashDot: BorderStyleTypes.MEDIUM_DASH_DOT,
  mediumDashDotDot: BorderStyleTypes.MEDIUM_DASH_DOT_DOT,
  slantDashDot: BorderStyleTypes.SLANT_DASH_DOT,
  thick: BorderStyleTypes.THICK,
};

function mapBorderSide(
  side: ParsedBorderSide | undefined,
): { s: BorderStyleTypes; cl: { rgb: string } } | undefined {
  if (!side) return undefined;
  const s = OOXML_BORDER_STYLE_MAP[side.style];
  if (s === undefined) return undefined;
  return {
    s,
    cl: { rgb: side.color ? `#${side.color}` : "#000000" },
  };
}

/**
 * Converts a SheetJS WorkBook to Univer IWorkbookData format.
 * Optionally accepts font styles extracted directly from XLSX XML
 * (bypassing SheetJS CE's limited style support).
 */
export function sheetjsToUniver(
  workbook: WorkBook,
  fontStyles?: XlsxCellStyleMap,
  freezePanes?: Map<string, ParsedFreezePane>,
): IWorkbookData {
  const styles: Record<string, IStyleData> = {};
  let styleCounter = 0;
  const styleCache = new Map<string, string>();

  function getOrCreateStyleId(style: IStyleData): string {
    const key = JSON.stringify(style);
    const existing = styleCache.get(key);
    if (existing) return existing;
    const id = `s${styleCounter++}`;
    styles[id] = style;
    styleCache.set(key, id);
    return id;
  }

  // Build a default style from the workbook's default font (font index 0)
  let defaultStyleId: string | undefined;
  const defaultFont = fontStyles?.defaultFont;
  if (defaultFont) {
    const defaultStyle: IStyleData = {};
    if (defaultFont.size) defaultStyle.fs = defaultFont.size;
    if (defaultFont.name) defaultStyle.ff = resolveFont(defaultFont.name);
    if (defaultFont.bold) defaultStyle.bl = BooleanNumber.TRUE;
    if (defaultFont.italic) defaultStyle.it = BooleanNumber.TRUE;
    if (Object.keys(defaultStyle).length > 0) {
      defaultStyleId = getOrCreateStyleId(defaultStyle);
    }
  }

  const sheets: Record<string, Partial<IWorksheetData>> = {};
  const sheetOrder: string[] = [];

  for (const sheetName of workbook.SheetNames) {
    const worksheet = workbook.Sheets[sheetName];
    if (!worksheet) continue;

    const sheetId = `sheet-${sheetOrder.length}`;
    sheetOrder.push(sheetId);

    const sheetFontStyles = fontStyles?.get(sheetName);
    const freezePane = freezePanes?.get(sheetName);

    sheets[sheetId] = convertSheet(
      worksheet,
      sheetName,
      sheetId,
      getOrCreateStyleId,
      sheetFontStyles,
      defaultStyleId,
      defaultFont,
      freezePane,
    );
  }

  return {
    id: "workbook-1",
    name: workbook.Props?.Title ?? "Workbook",
    appVersion: "1.0.0",
    locale: LocaleType.EN_US,
    styles,
    sheetOrder,
    sheets,
  };
}

function convertSheet(
  ws: WorkSheet,
  name: string,
  id: string,
  getOrCreateStyleId: (style: IStyleData) => string,
  sheetFontStyles?: Map<string, ParsedCellStyle>,
  defaultStyleId?: string,
  defaultFont?: ParsedCellStyle,
  freezePane?: ParsedFreezePane,
): Partial<IWorksheetData> {
  const refString = ws["!ref"] || "A1";
  const range = XLSX.utils.decode_range(refString);

  const rowCount = range.e.r + 1;
  const columnCount = range.e.c + 1;

  // Convert cells
  const cellData: Record<number, Record<number, ICellData>> = {};

  const cellAddresses = Object.keys(ws).filter((k) => !k.startsWith("!"));
  for (const addr of cellAddresses) {
    const cell = ws[addr];
    if (!cell) continue;

    const { r, c } = XLSX.utils.decode_cell(addr);
    if (!cellData[r]) cellData[r] = {};

    const univerCell: ICellData = {};

    // Value
    if (cell.v !== undefined && cell.v !== null) {
      univerCell.v = cell.v;
    }

    // Type
    if (cell.t === "n") {
      univerCell.t = CellValueType.NUMBER;
    } else if (cell.t === "b") {
      univerCell.t = CellValueType.BOOLEAN;
    } else if (cell.t === "s" || cell.t === "z") {
      univerCell.t = CellValueType.STRING;
    }

    // Formula
    if (cell.f) {
      univerCell.f = `=${cell.f}`;
    }

    // Style — combine SheetJS numfmt with custom-parsed style data
    const numFmt = cell.z;
    const fontData = sheetFontStyles?.get(addr);

    const style: IStyleData = {};
    let hasStyle = false;

    // Number format (from SheetJS)
    if (numFmt && numFmt !== "General") {
      style.n = { pattern: numFmt };
      hasStyle = true;
    }

    // Custom-parsed cell styles (fills, borders, alignment, fonts)
    if (fontData) {
      // Background fill color
      if (fontData.bg) {
        style.bg = { rgb: `#${fontData.bg}` };
        hasStyle = true;
      }

      // Font properties
      if (fontData.color) {
        style.cl = { rgb: `#${fontData.color}` };
        hasStyle = true;
      }
      if (fontData.bold) {
        style.bl = BooleanNumber.TRUE;
        hasStyle = true;
      }
      if (fontData.italic) {
        style.it = BooleanNumber.TRUE;
        hasStyle = true;
      }
      if (fontData.size) {
        style.fs = fontData.size;
        hasStyle = true;
      }
      if (fontData.name) {
        style.ff = resolveFont(fontData.name);
        hasStyle = true;
      }
      if (fontData.underline) {
        style.ul = { s: BooleanNumber.TRUE };
        hasStyle = true;
      }
      if (fontData.strike) {
        style.st = { s: BooleanNumber.TRUE };
        hasStyle = true;
      }

      // Borders
      const bl = mapBorderSide(fontData.borderLeft);
      const br = mapBorderSide(fontData.borderRight);
      const bt = mapBorderSide(fontData.borderTop);
      const bb = mapBorderSide(fontData.borderBottom);
      if (bl || br || bt || bb) {
        style.bd = {};
        if (bt) style.bd.t = bt;
        if (bb) style.bd.b = bb;
        if (bl) style.bd.l = bl;
        if (br) style.bd.r = br;
        hasStyle = true;
      }

      // Alignment
      if (fontData.horizontalAlign) {
        const ht = mapHorizontalAlign(fontData.horizontalAlign);
        if (ht !== undefined) {
          style.ht = ht;
          hasStyle = true;
        }
      }
      if (fontData.verticalAlign) {
        const vt = mapVerticalAlign(fontData.verticalAlign);
        if (vt !== undefined) {
          style.vt = vt;
          hasStyle = true;
        }
      }

      // Text wrapping
      if (fontData.wrapText) {
        style.tb = WrapStrategy.WRAP;
        hasStyle = true;
      }
      if (fontData.textRotation !== undefined || fontData.verticalText) {
        style.tr = {
          a: fontData.textRotation ?? 0,
          ...(fontData.verticalText ? { v: BooleanNumber.TRUE } : {}),
        };
        hasStyle = true;
      }
    }

    // Fill in workbook default font properties wherever the cell-level
    // style didn't set them. Cells with fontId 0 that have fills/borders
    // produce a fontData entry but no font properties — without this
    // fallback those cells would get Univer's built-in serif default.
    if (defaultFont) {
      if (!style.bl && defaultFont.bold) {
        style.bl = BooleanNumber.TRUE;
        hasStyle = true;
      }
      if (!style.it && defaultFont.italic) {
        style.it = BooleanNumber.TRUE;
        hasStyle = true;
      }
      if (!style.fs && defaultFont.size) {
        style.fs = defaultFont.size;
        hasStyle = true;
      }
      if (!style.ff && defaultFont.name) {
        style.ff = resolveFont(defaultFont.name);
        hasStyle = true;
      }
    }

    // Fall back to SheetJS bg color if custom parser didn't find one
    if (!fontData?.bg) {
      const bgColor = cell.s?.fill?.fgColor?.rgb || cell.s?.fgColor?.rgb;
      if (bgColor) {
        style.bg = { rgb: `#${bgColor}` };
        hasStyle = true;
      }
    }

    if (hasStyle) {
      univerCell.s = getOrCreateStyleId(style);
    }

    cellData[r][c] = univerCell;
  }

  // Merged cells
  const mergeData: IRange[] = (ws["!merges"] ?? []).map((m: XLSX.Range) => ({
    startRow: m.s.r,
    startColumn: m.s.c,
    endRow: m.e.r,
    endColumn: m.e.c,
  }));

  // Row data (hidden state + explicit heights)
  const rowData: Record<number, Partial<IRowData>> = {};
  if (ws["!rows"]) {
    ws["!rows"].forEach((row: XLSX.RowInfo, idx: number) => {
      if (!row) return;
      const data: Partial<IRowData> = {};
      if (row.hidden) data.hd = BooleanNumber.TRUE;
      const height = row.hpx ?? (row.hpt ? row.hpt * 1.33 : undefined);
      if (height) data.h = height * 1.2;
      if (Object.keys(data).length > 0) rowData[idx] = data;
    });
  }

  // Column data (widths — scaled by 1.3x because SheetJS
  // character-unit-to-pixel conversion underestimates vs. Excel)
  const columnData: Record<number, Partial<IColumnData>> = {};
  if (ws["!cols"]) {
    ws["!cols"].forEach((col: XLSX.ColInfo, idx: number) => {
      if (!col) return;
      const data: Partial<IColumnData> = {};
      if (col.wpx) data.w = col.wpx * 1.3;
      else if (col.wch) data.w = col.wch * 9.1;
      if (col.hidden) data.hd = BooleanNumber.TRUE;
      if (Object.keys(data).length > 0) columnData[idx] = data;
    });
  }

  const sheetData: Partial<IWorksheetData> = {
    id,
    name,
    tabColor: "",
    hidden: BooleanNumber.FALSE,
    freeze: freezePane
      ? {
          xSplit: freezePane.xSplit,
          ySplit: freezePane.ySplit,
          startRow: freezePane.ySplit > 0 ? freezePane.ySplit : -1,
          startColumn: freezePane.xSplit > 0 ? freezePane.xSplit : -1,
        }
      : { xSplit: 0, ySplit: 0, startRow: -1, startColumn: -1 },
    rowCount: Math.max(rowCount, 100),
    columnCount: Math.max(columnCount, 26),
    defaultColumnWidth: 88,
    defaultRowHeight: 24,
    mergeData,
    cellData,
    rowData,
    columnData,
    rowHeader: { width: 46 },
    columnHeader: { height: 20 },
    showGridlines: BooleanNumber.TRUE,
    rightToLeft: BooleanNumber.FALSE,
    zoomRatio: 1,
    scrollTop: 0,
    scrollLeft: 0,
  };

  if (defaultStyleId) {
    sheetData.defaultStyle = defaultStyleId;
  }

  return sheetData;
}

/**
 * Converts Univer workbook data back to a SheetJS WorkBook for export.
 */
export function univerToSheetjs(workbookData: IWorkbookData): WorkBook {
  const wb = XLSX.utils.book_new();

  for (const sheetId of workbookData.sheetOrder) {
    const sheetData = workbookData.sheets[sheetId];
    if (!sheetData) continue;

    const ws: WorkSheet = {};
    const cellMatrix = sheetData.cellData;

    let maxRow = 0;
    let maxCol = 0;

    if (cellMatrix) {
      for (const rowStr of Object.keys(cellMatrix)) {
        const r = Number(rowStr);
        const row = cellMatrix[r];
        if (!row) continue;
        for (const colStr of Object.keys(row)) {
          const c = Number(colStr);
          const cell = row[c];
          if (!cell) continue;

          maxRow = Math.max(maxRow, r);
          maxCol = Math.max(maxCol, c);

          const addr = XLSX.utils.encode_cell({ r, c });
          const sheetCell: XLSX.CellObject = {
            v: cell.v ?? "",
            t:
              cell.t === CellValueType.NUMBER
                ? "n"
                : cell.t === CellValueType.BOOLEAN
                  ? "b"
                  : "s",
          };

          if (cell.f) {
            // Strip leading = since SheetJS stores without it
            sheetCell.f = cell.f.startsWith("=") ? cell.f.slice(1) : cell.f;
          }

          ws[addr] = sheetCell;
        }
      }
    }

    ws["!ref"] = XLSX.utils.encode_range({
      s: { r: 0, c: 0 },
      e: { r: maxRow, c: maxCol },
    });

    // Merged cells
    if (sheetData.mergeData && sheetData.mergeData.length > 0) {
      ws["!merges"] = sheetData.mergeData.map((m) => ({
        s: { r: m.startRow, c: m.startColumn },
        e: { r: m.endRow, c: m.endColumn },
      }));
    }

    XLSX.utils.book_append_sheet(
      wb,
      ws,
      sheetData.name ?? `Sheet${workbookData.sheetOrder.indexOf(sheetId) + 1}`,
    );
  }

  return wb;
}
