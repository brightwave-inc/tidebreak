import JSZip from "jszip";

const OFFICE_DOCUMENT_RELATIONSHIPS =
  "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const DRAWINGML =
  "http://schemas.openxmlformats.org/drawingml/2006/main";
const DRAWINGML_CHART =
  "http://schemas.openxmlformats.org/drawingml/2006/chart";
const SPREADSHEETML =
  "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

export interface ReadOnlyConditionalCellStyle {
  backgroundColor?: string;
  dataBar?: {
    color: string;
    widthPercent: number;
  };
}

export interface ReadOnlyWorkbookProjection {
  conditionalStylesBySheet: Record<
    number,
    Record<string, ReadOnlyConditionalCellStyle>
  >;
  data: ArrayBuffer;
  formulasBySheet: Record<number, Record<string, string>>;
}

/**
 * Build a display-only copy of an OOXML workbook.
 *
 * Excel stores both a formula and its last calculated value. Browser formula
 * engines cannot reproduce every Excel function, but a viewer does not need
 * to: it can paint the cached result with the cell's original number format
 * and keep the formula separately for inspection. The source bytes are never
 * changed or written back.
 */
export async function projectWorkbookForReadOnlyDisplay(
  source: ArrayBuffer,
): Promise<ReadOnlyWorkbookProjection> {
  try {
    return await projectWorkbookDisplayCopy(source);
  } catch {
    // The display copy is an enhancement. A part the browser XML parser
    // rejects must not block reading the original workbook.
    return emptyProjection(source);
  }
}

async function projectWorkbookDisplayCopy(
  source: ArrayBuffer,
): Promise<ReadOnlyWorkbookProjection> {
  if (!isZip(source)) {
    return emptyProjection(source);
  }

  const zip = await JSZip.loadAsync(source);
  const sheetPaths = await readWorkbookSheetPaths(zip);
  const conditionalStylesBySheet: Record<
    number,
    Record<string, ReadOnlyConditionalCellStyle>
  > = {};
  const formulasBySheet: Record<number, Record<string, string>> = {};
  let changed = await normalizeEmptyBorders(zip);
  if (await normalizeImplicitChartSeriesColors(zip)) changed = true;

  await Promise.all(
    sheetPaths.map(async (path, sheetIndex) => {
      const entry = zip.file(path);
      if (!entry) return;

      const document = parseXml(await entry.async("string"), path);
      const formulas: Record<string, string> = {};
      const conditionalStyles = projectConditionalStyles(document);
      let sheetChanged = false;

      if (Object.keys(conditionalStyles).length > 0) {
        conditionalStylesBySheet[sheetIndex] = conditionalStyles;
      }

      for (const cell of localElements(document, "c")) {
        const formula = localChild(cell, "f");
        if (!formula) continue;

        const address = cell.getAttribute("r");
        const formulaText = formula.textContent?.trim();
        if (address && formulaText) formulas[address] = `=${formulaText}`;

        // Only replace formulas that have an authored cached result. Formulas
        // without one still go through the workbook engine as a best effort.
        if (localChild(cell, "v")) {
          cell.removeChild(formula);
          sheetChanged = true;
        }
      }

      if (Object.keys(formulas).length > 0) {
        formulasBySheet[sheetIndex] = formulas;
      }
      if (sheetChanged) {
        zip.file(path, new XMLSerializer().serializeToString(document));
        changed = true;
      }
    }),
  );

  if (!changed) {
    return { conditionalStylesBySheet, data: source, formulasBySheet };
  }
  return {
    conditionalStylesBySheet,
    data: await zip.generateAsync({
      type: "arraybuffer",
      compression: "DEFLATE",
      compressionOptions: { level: 6 },
    }),
    formulasBySheet,
  };
}

function projectConditionalStyles(
  document: XMLDocument,
): Record<string, ReadOnlyConditionalCellStyle> {
  const numericValues = new Map<string, number>();
  const styles: Record<string, ReadOnlyConditionalCellStyle> = {};

  for (const cell of localElements(document, "c")) {
    const address = cell.getAttribute("r")?.replaceAll("$", "").toUpperCase();
    const rawValue = localChild(cell, "v")?.textContent;
    const value =
      rawValue === null || rawValue === undefined
        ? Number.NaN
        : Number(rawValue);
    if (address && Number.isFinite(value)) numericValues.set(address, value);
  }

  for (const formatting of localElements(document, "conditionalFormatting")) {
    if (formatting.namespaceURI !== SPREADSHEETML) continue;
    const ranges = parseSqref(formatting.getAttribute("sqref"));
    if (ranges.length === 0) continue;

    const addresses = ranges.flatMap(cellsInRange);
    const values = addresses
      .map((address) => numericValues.get(address))
      .filter((value): value is number => value !== undefined);
    if (values.length === 0) continue;

    for (const rule of Array.from(formatting.children).filter(
      (child) => child.localName === "cfRule",
    )) {
      if (rule.getAttribute("type") === "dataBar") {
        applyDataBarStyles(rule, addresses, numericValues, values, styles);
      } else if (rule.getAttribute("type") === "colorScale") {
        applyColorScaleStyles(rule, addresses, numericValues, values, styles);
      }
    }
  }

  return styles;
}

function applyDataBarStyles(
  rule: Element,
  addresses: string[],
  numericValues: Map<string, number>,
  values: number[],
  styles: Record<string, ReadOnlyConditionalCellStyle>,
) {
  const dataBar = localChild(rule, "dataBar");
  if (!dataBar) return;
  const thresholds = Array.from(dataBar.children)
    .filter((child) => child.localName === "cfvo")
    .map((node) => resolveConditionalThreshold(node, values));
  const min = thresholds[0] ?? Math.min(...values);
  const max = thresholds.at(-1) ?? Math.max(...values);
  const color = ooxmlRgb(localChild(dataBar, "color"));
  if (!color) return;

  for (const address of addresses) {
    const value = numericValues.get(address);
    if (value === undefined) continue;
    const ratio =
      max === min ? (value >= max ? 1 : 0) : (value - min) / (max - min);
    styles[address] = {
      ...styles[address],
      dataBar: {
        color,
        widthPercent: Math.max(0, Math.min(100, ratio * 100)),
      },
    };
  }
}

function applyColorScaleStyles(
  rule: Element,
  addresses: string[],
  numericValues: Map<string, number>,
  values: number[],
  styles: Record<string, ReadOnlyConditionalCellStyle>,
) {
  const colorScale = localChild(rule, "colorScale");
  if (!colorScale) return;
  const thresholds = Array.from(colorScale.children)
    .filter((child) => child.localName === "cfvo")
    .map((node) => resolveConditionalThreshold(node, values));
  const colors = Array.from(colorScale.children)
    .filter((child) => child.localName === "color")
    .map(ooxmlRgb)
    .filter((color): color is string => color !== null);
  if (thresholds.length < 2 || colors.length < 2) return;

  const stopCount = Math.min(thresholds.length, colors.length);
  for (const address of addresses) {
    const value = numericValues.get(address);
    if (value === undefined) continue;
    styles[address] = {
      ...styles[address],
      backgroundColor: colorAtScale(
        value,
        thresholds.slice(0, stopCount),
        colors.slice(0, stopCount),
      ),
    };
  }
}

function resolveConditionalThreshold(node: Element, values: number[]): number {
  const sorted = [...values].sort((left, right) => left - right);
  const min = sorted[0] ?? 0;
  const max = sorted.at(-1) ?? min;
  const rawValue = Number(node.getAttribute("val") ?? Number.NaN);
  switch (node.getAttribute("type")) {
    case "max":
      return max;
    case "min":
      return min;
    case "num":
      return Number.isFinite(rawValue) ? rawValue : min;
    case "percent":
      return Number.isFinite(rawValue)
        ? min + (max - min) * (rawValue / 100)
        : min;
    case "percentile":
      return Number.isFinite(rawValue)
        ? percentile(sorted, rawValue / 100)
        : min;
    default:
      return Number.isFinite(rawValue) ? rawValue : min;
  }
}

function percentile(sorted: number[], ratio: number): number {
  if (sorted.length === 0) return 0;
  const index = Math.max(0, Math.min(1, ratio)) * (sorted.length - 1);
  const lower = Math.floor(index);
  const upper = Math.ceil(index);
  const weight = index - lower;
  return (sorted[lower] ?? 0) * (1 - weight) + (sorted[upper] ?? 0) * weight;
}

function colorAtScale(
  value: number,
  thresholds: number[],
  colors: string[],
): string {
  for (let index = 0; index < thresholds.length - 1; index += 1) {
    const start = thresholds[index]!;
    const end = thresholds[index + 1]!;
    if (value <= end || index === thresholds.length - 2) {
      const ratio = end === start ? 1 : (value - start) / (end - start);
      return mixRgb(colors[index]!, colors[index + 1]!, ratio);
    }
  }
  return colors.at(-1)!;
}

function mixRgb(start: string, end: string, rawRatio: number): string {
  const ratio = Math.max(0, Math.min(1, rawRatio));
  const startRgb = hexChannels(start);
  const endRgb = hexChannels(end);
  return `rgb(${startRgb
    .map((channel, index) =>
      Math.round(channel + (endRgb[index]! - channel) * ratio),
    )
    .join(", ")})`;
}

function hexChannels(color: string): [number, number, number] {
  const hex = color.replace(/^#/, "");
  return [0, 2, 4].map((index) =>
    Number.parseInt(hex.slice(index, index + 2), 16),
  ) as [number, number, number];
}

function ooxmlRgb(node: Element | null): string | null {
  const raw = node?.getAttribute("rgb")?.replace(/^#/, "") ?? "";
  const rgb = raw.length === 8 ? raw.slice(2) : raw;
  return /^[0-9a-f]{6}$/i.test(rgb) ? `#${rgb.toLowerCase()}` : null;
}

interface CellRange {
  end: { col: number; row: number };
  start: { col: number; row: number };
}

function parseSqref(value: string | null): CellRange[] {
  return (value ?? "")
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .map(parseCellRange)
    .filter((range): range is CellRange => range !== null);
}

function parseCellRange(value: string): CellRange | null {
  const [rawStart, rawEnd = rawStart] = value.split(":", 2);
  const start = rawStart ? parseCellAddress(rawStart) : null;
  const end = rawEnd ? parseCellAddress(rawEnd) : null;
  if (!start || !end) return null;
  return {
    end: { col: Math.max(start.col, end.col), row: Math.max(start.row, end.row) },
    start: { col: Math.min(start.col, end.col), row: Math.min(start.row, end.row) },
  };
}

function parseCellAddress(value: string): { col: number; row: number } | null {
  const match = value.replaceAll("$", "").match(/^([A-Z]+)(\d+)$/i);
  if (!match?.[1] || !match[2]) return null;
  let col = 0;
  for (const letter of match[1].toUpperCase()) {
    col = col * 26 + letter.charCodeAt(0) - 64;
  }
  const row = Number(match[2]) - 1;
  return row >= 0 ? { col: col - 1, row } : null;
}

function cellsInRange(range: CellRange): string[] {
  const addresses: string[] = [];
  for (let row = range.start.row; row <= range.end.row; row += 1) {
    for (let col = range.start.col; col <= range.end.col; col += 1) {
      addresses.push(cellAddressAt(row, col));
    }
  }
  return addresses;
}

function cellAddressAt(row: number, col: number): string {
  let column = col + 1;
  let letters = "";
  while (column > 0) {
    const remainder = (column - 1) % 26;
    letters = String.fromCharCode(65 + remainder) + letters;
    column = Math.floor((column - 1) / 26);
  }
  return `${letters}${row + 1}`;
}

/**
 * A chart without an explicit built-in style or per-series shape properties
 * inherits accent1, accent2, etc. from the workbook theme. The current worker
 * falls back to the stock Office palette before that theme reaches its chart
 * renderer, so make the inherited colors explicit in the display copy.
 */
async function normalizeImplicitChartSeriesColors(
  zip: JSZip,
): Promise<boolean> {
  const themePath = Object.keys(zip.files).find((path) =>
    /^xl\/theme\/theme\d+\.xml$/i.test(path),
  );
  const themeEntry = themePath ? zip.file(themePath) : null;
  if (!themeEntry) return false;

  const theme = parseXml(await themeEntry.async("string"), themePath!);
  const accents = [
    "accent1",
    "accent2",
    "accent3",
    "accent4",
    "accent5",
    "accent6",
  ].map((name) => themeColor(theme, name));
  const fallbackAccent = accents.find((color) => color !== null);
  if (!fallbackAccent) return false;

  let changed = false;
  const chartPaths = Object.keys(zip.files).filter((path) =>
    /(?:^|\/)charts\/chart\d+\.xml$/i.test(path),
  );

  await Promise.all(
    chartPaths.map(async (path) => {
      const entry = zip.file(path);
      if (!entry) return;

      const document = parseXml(await entry.async("string"), path);
      const hasBuiltInStyle = localElements(document, "style").some(
        (element) => element.namespaceURI === DRAWINGML_CHART,
      );
      if (hasBuiltInStyle) return;

      let chartChanged = false;
      for (const [seriesIndex, series] of localElements(
        document,
        "ser",
      ).entries()) {
        if (localChild(series, "spPr") || chartVariesPointColors(series)) {
          continue;
        }

        const color = accents[seriesIndex % accents.length] ?? fallbackAccent;
        series.insertBefore(
          chartSeriesShapeProperties(document, series, color),
          chartSeriesStyleInsertionPoint(series),
        );
        chartChanged = true;
      }

      if (chartChanged) {
        zip.file(path, new XMLSerializer().serializeToString(document));
        changed = true;
      }
    }),
  );

  return changed;
}

function themeColor(document: XMLDocument, name: string): string | null {
  const slot = localElements(document, name)[0];
  if (!slot) return null;
  const color = Array.from(slot.children).find(
    (child) => child.localName === "srgbClr" || child.localName === "sysClr",
  );
  const hex =
    color?.localName === "sysClr"
      ? color.getAttribute("lastClr")
      : (color?.getAttribute("val") ?? null);
  return hex && /^[0-9a-f]{6}$/i.test(hex) ? hex.toUpperCase() : null;
}

function chartVariesPointColors(series: Element): boolean {
  const varies = series.parentElement
    ? localChild(series.parentElement, "varyColors")
    : null;
  const value = varies?.getAttribute("val")?.toLowerCase();
  return value === "1" || value === "true";
}

function chartSeriesShapeProperties(
  document: XMLDocument,
  series: Element,
  color: string,
): Element {
  const chartPrefix = series.prefix ?? "c";
  const shape = document.createElementNS(
    DRAWINGML_CHART,
    `${chartPrefix}:spPr`,
  );
  shape.appendChild(solidDrawingFill(document, color));

  const line = document.createElementNS(DRAWINGML, "a:ln");
  line.appendChild(solidDrawingFill(document, color));
  shape.appendChild(line);
  return shape;
}

function solidDrawingFill(document: XMLDocument, color: string): Element {
  const fill = document.createElementNS(DRAWINGML, "a:solidFill");
  const rgb = document.createElementNS(DRAWINGML, "a:srgbClr");
  rgb.setAttribute("val", color);
  fill.appendChild(rgb);
  return fill;
}

function chartSeriesStyleInsertionPoint(series: Element): Element | null {
  return (
    Array.from(series.children).find(
      (child) => !["idx", "order", "tx"].includes(child.localName),
    ) ?? null
  );
}

/**
 * Some producers serialize the default OOXML border as a self-closing
 * `<border/>`. Duke currently omits that record while decoding styles, which
 * shifts every later border index and paints the wrong border on each cell.
 * Expanding the empty record is semantically identical OOXML and keeps the
 * style table indices stable for the read-only renderer.
 */
async function normalizeEmptyBorders(zip: JSZip): Promise<boolean> {
  const entry = zip.file("xl/styles.xml");
  if (!entry) return false;

  const document = parseXml(await entry.async("string"), "xl/styles.xml");
  let changed = false;

  for (const borders of localElements(document, "borders")) {
    for (const border of Array.from(borders.children)) {
      if (border.localName !== "border" || border.children.length > 0) continue;

      for (const edge of ["left", "right", "top", "bottom", "diagonal"]) {
        border.appendChild(namespacedSibling(document, border, edge));
      }
      changed = true;
    }
  }

  if (changed) {
    zip.file("xl/styles.xml", new XMLSerializer().serializeToString(document));
  }
  return changed;
}

function namespacedSibling(
  document: XMLDocument,
  sibling: Element,
  localName: string,
): Element {
  const qualifiedName = sibling.prefix
    ? `${sibling.prefix}:${localName}`
    : localName;
  return document.createElementNS(sibling.namespaceURI, qualifiedName);
}

async function readWorkbookSheetPaths(zip: JSZip): Promise<string[]> {
  const workbookEntry = zip.file("xl/workbook.xml");
  const relationshipsEntry = zip.file("xl/_rels/workbook.xml.rels");
  if (!workbookEntry || !relationshipsEntry) return fallbackSheetPaths(zip);

  const [workbookDocument, relationshipsDocument] = await Promise.all([
    workbookEntry
      .async("string")
      .then((xml) => parseXml(xml, "xl/workbook.xml")),
    relationshipsEntry
      .async("string")
      .then((xml) => parseXml(xml, "xl/_rels/workbook.xml.rels")),
  ]);
  const targetByRelationship = new Map<string, string>();

  for (const relationship of localElements(
    relationshipsDocument,
    "Relationship",
  )) {
    const id = relationship.getAttribute("Id");
    const target = relationship.getAttribute("Target");
    if (id && target) {
      targetByRelationship.set(
        id,
        resolveZipPath("xl/workbook.xml", target),
      );
    }
  }

  return localElements(workbookDocument, "sheet")
    .map((sheet) => {
      const relationshipId =
        sheet.getAttributeNS(OFFICE_DOCUMENT_RELATIONSHIPS, "id") ??
        sheet.getAttribute("r:id");
      return relationshipId
        ? (targetByRelationship.get(relationshipId) ?? null)
        : null;
    })
    .filter((path): path is string => path !== null);
}

function fallbackSheetPaths(zip: JSZip): string[] {
  return Object.keys(zip.files)
    .filter((path) => /^xl\/worksheets\/sheet\d+\.xml$/i.test(path))
    .sort((left, right) => sheetNumber(left) - sheetNumber(right));
}

function sheetNumber(path: string): number {
  return Number(path.match(/sheet(\d+)\.xml$/i)?.[1] ?? Number.MAX_SAFE_INTEGER);
}

function resolveZipPath(baseFile: string, target: string): string {
  if (target.startsWith("/")) return target.replace(/^\/+/, "");
  const parts = `${baseFile.slice(0, baseFile.lastIndexOf("/") + 1)}${target}`
    .split("/")
    .filter(Boolean);
  const resolved: string[] = [];
  for (const part of parts) {
    if (part === ".") continue;
    if (part === "..") resolved.pop();
    else resolved.push(part);
  }
  return resolved.join("/");
}

function parseXml(xml: string, path: string): XMLDocument {
  // JSZip decodes a UTF-8 BOM into U+FEFF. WebKit then treats that mark as
  // content before `<?xml` and refuses the part (`XML declaration allowed
  // only at the start of the document`). Excel's packaging often writes a
  // BOM on `[Content_Types].xml` and every `.rels` file.
  const document = new DOMParser().parseFromString(
    xml.replace(/^\uFEFF+/, ""),
    "application/xml",
  );
  if (document.getElementsByTagName("parsererror").length > 0) {
    throw new Error(`Could not parse ${path}`);
  }
  return document;
}

function emptyProjection(data: ArrayBuffer): ReadOnlyWorkbookProjection {
  return { conditionalStylesBySheet: {}, data, formulasBySheet: {} };
}

function localElements(document: Document | Element, name: string): Element[] {
  return Array.from(document.getElementsByTagNameNS("*", name));
}

function localChild(element: Element, name: string): Element | null {
  return (
    Array.from(element.children).find((child) => child.localName === name) ??
    null
  );
}

function isZip(data: ArrayBuffer): boolean {
  const bytes = new Uint8Array(data, 0, Math.min(data.byteLength, 4));
  return (
    bytes.length === 4 &&
    bytes[0] === 0x50 &&
    bytes[1] === 0x4b &&
    (bytes[2] === 0x03 || bytes[2] === 0x05 || bytes[2] === 0x07) &&
    (bytes[3] === 0x04 || bytes[3] === 0x06 || bytes[3] === 0x08)
  );
}
