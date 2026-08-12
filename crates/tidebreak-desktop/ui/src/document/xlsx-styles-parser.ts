import JSZip from "jszip";

// ── Types ────────────────────────────────────────────────────────────────────

export interface ParsedBorderSide {
    style: string; // OOXML border style name (thin, medium, thick, etc.)
    color?: string; // Resolved RGB hex without # (e.g. "FF0000")
}

export interface ParsedCellStyle {
    color?: string; // Resolved RGB hex without # (e.g. "FF0000")
    bold?: boolean;
    italic?: boolean;
    size?: number;
    name?: string;
    underline?: boolean;
    strike?: boolean;
    // Cell-level styles (parsed from cellXfs fills, borders, alignment)
    bg?: string; // Background fill color hex without #
    borderLeft?: ParsedBorderSide;
    borderRight?: ParsedBorderSide;
    borderTop?: ParsedBorderSide;
    borderBottom?: ParsedBorderSide;
    horizontalAlign?: string;
    verticalAlign?: string;
    wrapText?: boolean;
    textRotation?: number;
    verticalText?: boolean;
}

export interface ParsedFreezePane {
    xSplit: number; // Number of frozen columns
    ySplit: number; // Number of frozen rows
}

/** Map<sheetName, Map<cellAddress, ParsedCellStyle>> */
export type XlsxCellStyleMap = Map<string, Map<string, ParsedCellStyle>> & {
    defaultFont?: ParsedCellStyle;
};

// ── Regex-based XML helpers (web-worker-safe, no DOMParser) ──────────────────

/** Find all occurrences of a tag, returning {tag, inner} for body tags and {tag, inner:""} for self-closing */
function findTags(
    xml: string,
    tagName: string,
): { tag: string; inner: string }[] {
    // Self-closing FIRST (with lazy [^>]*? so the / before > isn't consumed),
    // then body tags.  The old regex used greedy [^>]* which ate the trailing /
    // in self-closing tags, making them unmatchable; the body alternative then
    // swallowed them + the next closing tag, shifting every subsequent index.
    const pattern = new RegExp(
        `<${tagName}\\b[^>]*?\\/>|<${tagName}\\b[^>]*?>([\\s\\S]*?)<\\/${tagName}>`,
        "g",
    );
    const results: { tag: string; inner: string }[] = [];
    let m: RegExpExecArray | null;
    while ((m = pattern.exec(xml)) !== null) {
        results.push({ tag: m[0], inner: m[1] ?? "" });
    }
    return results;
}

/** Get inner content between opening and closing tags */
function getInner(xml: string, tagName: string): string | null {
    const m = xml.match(
        new RegExp(`<${tagName}(?:\\s[^>]*)?>([\\s\\S]*?)<\\/${tagName}>`),
    );
    return m ? m[1]! : null;
}

/** Get attribute value from a tag string */
function getAttr(tag: string, attr: string): string | null {
    const m = tag.match(new RegExp(`${attr}="([^"]*)"`));
    return m ? m[1]! : null;
}

/** Check if a flag element exists and isn't explicitly val="0" */
function hasFlag(xml: string, tagName: string): boolean {
    const m = xml.match(new RegExp(`<${tagName}(?:\\s[^>]*)?\\/?>`, "i"));
    if (!m) return false;
    const val = getAttr(m[0], "val");
    return val !== "0" && val !== "false";
}

/** Get the val attribute from a child element */
function childAttrVal(xml: string, tagName: string): string | null {
    const m = xml.match(new RegExp(`<${tagName}\\s[^>]*?\\/?>`));
    if (!m) return null;
    return getAttr(m[0], "val");
}

// ── Theme color parsing ─────────────────────────────────────────────────────

// OOXML theme indices are swapped for the first two pairs:
// dk1→index 1, lt1→index 0, dk2→index 3, lt2→index 2, accent1-6→indices 4-9
const THEME_INDEX_MAP: Record<number, number> = {
    0: 1, // dk1
    1: 0, // lt1
    2: 3, // dk2
    3: 2, // lt2
};

const DEFAULT_THEME_COLORS: Record<number, string> = {
    0: "000000", // dk1
    1: "FFFFFF", // lt1
    2: "44546A", // dk2
    3: "E7E6E6", // lt2
    4: "4472C4", // accent1
    5: "ED7D31", // accent2
    6: "A5A5A5", // accent3
    7: "FFC000", // accent4
    8: "5B9BD5", // accent5
    9: "70AD47", // accent6
};

function parseThemeColors(themeXml: string): string[] {
    const colors: string[] = [];

    const themeElements = getInner(themeXml, "a:themeElements");
    if (!themeElements) return colors;

    const clrScheme = getInner(themeElements, "a:clrScheme");
    if (!clrScheme) return colors;

    // Parse color elements in order: dk1, lt1, dk2, lt2, accent1-6
    const colorTags = [
        "a:dk1",
        "a:lt1",
        "a:dk2",
        "a:lt2",
        "a:accent1",
        "a:accent2",
        "a:accent3",
        "a:accent4",
        "a:accent5",
        "a:accent6",
    ];

    for (const tag of colorTags) {
        const inner = getInner(clrScheme, tag);
        if (!inner) {
            colors.push("");
            continue;
        }

        // Try srgbClr first, then sysClr lastClr
        const srgb = inner.match(/<a:srgbClr\s[^>]*?val="([^"]+)"/);
        if (srgb) {
            colors.push(srgb[1]!);
            continue;
        }

        const sys = inner.match(/<a:sysClr\s[^>]*?lastClr="([^"]+)"/);
        if (sys) {
            colors.push(sys[1]!);
            continue;
        }

        colors.push("");
    }

    return colors;
}

function resolveThemeColor(
    themeIndex: number,
    themeColors: string[],
): string | null {
    const mappedIndex = THEME_INDEX_MAP[themeIndex] ?? themeIndex;
    const color = themeColors[mappedIndex];
    if (color) return color;
    return DEFAULT_THEME_COLORS[themeIndex] ?? null;
}

// ── Color tint helpers ──────────────────────────────────────────────────────

function applyTint(hex: string, tint: number): string {
    const r = parseInt(hex.substring(0, 2), 16);
    const g = parseInt(hex.substring(2, 4), 16);
    const b = parseInt(hex.substring(4, 6), 16);

    const apply = (c: number) => {
        if (tint < 0) return Math.round(c * (1 + tint));
        return Math.round(c + (255 - c) * tint);
    };

    const clamp = (v: number) => Math.min(255, Math.max(0, v));

    return [clamp(apply(r)), clamp(apply(g)), clamp(apply(b))]
        .map((v) => v.toString(16).padStart(2, "0").toUpperCase())
        .join("");
}

/** Resolve color from a tag like <color rgb="..." /> or <color theme="..." tint="..." /> */
function resolveColor(
    colorTag: string,
    themeColors: string[],
): string | undefined {
    const rgb = getAttr(colorTag, "rgb");
    if (rgb) {
        const hex = rgb.length === 8 ? rgb.slice(2) : rgb;
        const tint = getAttr(colorTag, "tint");
        return tint ? applyTint(hex, parseFloat(tint)) : hex;
    }

    const theme = getAttr(colorTag, "theme");
    if (theme !== null) {
        const resolved = resolveThemeColor(parseInt(theme, 10), themeColors);
        if (resolved) {
            const tint = getAttr(colorTag, "tint");
            return tint ? applyTint(resolved, parseFloat(tint)) : resolved;
        }
    }

    return undefined;
}

// ── Font parsing ────────────────────────────────────────────────────────────

interface FontDef {
    color?: string;
    bold?: boolean;
    italic?: boolean;
    size?: number;
    name?: string;
    underline?: boolean;
    strike?: boolean;
}

function parseFonts(stylesXml: string, themeColors: string[]): FontDef[] {
    const fontsBlock = getInner(stylesXml, "fonts");
    if (!fontsBlock) return [];

    const fontTags = findTags(fontsBlock, "font");
    const fonts: FontDef[] = [];

    for (const { inner } of fontTags) {
        const font: FontDef = {};

        const colorMatch = inner.match(/<color\s[^>]*?\/?>/);
        if (colorMatch) {
            const resolved = resolveColor(colorMatch[0], themeColors);
            if (resolved) font.color = resolved;
        }

        if (hasFlag(inner, "b")) font.bold = true;
        if (hasFlag(inner, "i")) font.italic = true;
        if (hasFlag(inner, "u")) font.underline = true;
        if (hasFlag(inner, "strike")) font.strike = true;

        const sz = childAttrVal(inner, "sz");
        if (sz) font.size = parseFloat(sz);

        const nameVal = childAttrVal(inner, "name");
        if (nameVal) font.name = nameVal;

        fonts.push(font);
    }

    return fonts;
}

// ── Fill parsing ─────────────────────────────────────────────────────────────

interface FillDef {
    color?: string; // Resolved RGB hex for solid fills
}

function parseFills(stylesXml: string, themeColors: string[]): FillDef[] {
    const fillsBlock = getInner(stylesXml, "fills");
    if (!fillsBlock) return [];

    const fillTags = findTags(fillsBlock, "fill");
    const fills: FillDef[] = [];

    for (const { inner } of fillTags) {
        const fill: FillDef = {};

        const patternFillMatch = inner.match(/<patternFill[^>]*>/);
        if (patternFillMatch) {
            const patternType = getAttr(patternFillMatch[0], "patternType");
            if (patternType === "solid") {
                const fgColorMatch = inner.match(/<fgColor\s[^>]*?\/?>/);
                if (fgColorMatch) {
                    fill.color = resolveColor(fgColorMatch[0], themeColors);
                }
            }
        }

        fills.push(fill);
    }

    return fills;
}

// ── Border parsing ───────────────────────────────────────────────────────────

interface BorderSideDef {
    style: string;
    color?: string;
}

interface BorderDef {
    left?: BorderSideDef;
    right?: BorderSideDef;
    top?: BorderSideDef;
    bottom?: BorderSideDef;
}

function parseBorderSide(
    borderXml: string,
    sideTag: string,
    themeColors: string[],
): ParsedBorderSide | undefined {
    const found = findTags(borderXml, sideTag);
    if (found.length === 0) return undefined;

    const { tag } = found[0]!;
    const style = getAttr(tag, "style");
    if (!style || style === "none") return undefined;

    const side: ParsedBorderSide = { style };

    const colorMatch = tag.match(/<color\s[^>]*?\/?>/);
    if (colorMatch) {
        side.color = resolveColor(colorMatch[0], themeColors);
    }

    return side;
}

function parseBorders(stylesXml: string, themeColors: string[]): BorderDef[] {
    const bordersBlock = getInner(stylesXml, "borders");
    if (!bordersBlock) return [];

    const borderTags = findTags(bordersBlock, "border");
    const borders: BorderDef[] = [];

    for (const { inner } of borderTags) {
        borders.push({
            left: parseBorderSide(inner, "left", themeColors),
            right: parseBorderSide(inner, "right", themeColors),
            top: parseBorderSide(inner, "top", themeColors),
            bottom: parseBorderSide(inner, "bottom", themeColors),
        });
    }

    return borders;
}

// ── cellXfs parsing (cell format → font/fill/border/alignment mapping) ──────

interface CellXf {
    fontId: number;
    fillId: number;
    borderId: number;
    alignment?: {
        horizontal?: string;
        vertical?: string;
        wrapText?: boolean;
        textRotation?: number;
        verticalText?: boolean;
    };
}

function parseCellXfs(stylesXml: string): CellXf[] {
    const cellXfsBlock = getInner(stylesXml, "cellXfs");
    if (!cellXfsBlock) return [];

    const xfTags = findTags(cellXfsBlock, "xf");
    return xfTags.map(({ tag, inner }) => {
        const fontId = getAttr(tag, "fontId");
        const fillId = getAttr(tag, "fillId");
        const borderId = getAttr(tag, "borderId");

        const xf: CellXf = {
            fontId: fontId ? parseInt(fontId, 10) : 0,
            fillId: fillId ? parseInt(fillId, 10) : 0,
            borderId: borderId ? parseInt(borderId, 10) : 0,
        };

        const alignMatch = inner.match(/<alignment\s[^>]*?\/?>/);
        if (alignMatch) {
            const horizontal = getAttr(alignMatch[0], "horizontal");
            const vertical = getAttr(alignMatch[0], "vertical");
            const wrapText = getAttr(alignMatch[0], "wrapText");
            const textRotation = getAttr(alignMatch[0], "textRotation");
            const parsedTextRotation = textRotation
                ? Number.parseInt(textRotation, 10)
                : null;
            const validTextRotation = Number.isFinite(parsedTextRotation)
                ? parsedTextRotation
                : null;

            if (
                horizontal ||
                vertical ||
                wrapText === "1" ||
                wrapText === "true" ||
                validTextRotation !== null
            ) {
                xf.alignment = {};
                if (horizontal) xf.alignment.horizontal = horizontal;
                if (vertical) xf.alignment.vertical = vertical;
                if (wrapText === "1" || wrapText === "true")
                    xf.alignment.wrapText = true;
                if (validTextRotation === 255) {
                    xf.alignment.verticalText = true;
                } else if (validTextRotation !== null) {
                    xf.alignment.textRotation = validTextRotation;
                }
            }
        }

        return xf;
    });
}

// ── Sheet path resolution ───────────────────────────────────────────────────

interface SheetInfo {
    name: string;
    rId: string;
}

function parseWorkbookSheets(workbookXml: string): SheetInfo[] {
    const sheets: SheetInfo[] = [];
    const sheetPattern = /<sheet\s[^>]*?\/?>/g;
    let m: RegExpExecArray | null;
    while ((m = sheetPattern.exec(workbookXml)) !== null) {
        const name = getAttr(m[0], "name");
        const rId = m[0].match(/r:id="([^"]+)"/)?.[1];
        if (name && rId) sheets.push({ name, rId });
    }
    return sheets;
}

function parseWorkbookRels(relsXml: string): Map<string, string> {
    const map = new Map<string, string>();
    const relPattern = /<Relationship\s[^>]*?\/?>/g;
    let m: RegExpExecArray | null;
    while ((m = relPattern.exec(relsXml)) !== null) {
        const id = getAttr(m[0], "Id");
        const target = getAttr(m[0], "Target");
        if (id && target) map.set(id, target);
    }
    return map;
}

// ── Per-sheet cell → font extraction ────────────────────────────────────────

function parseSheetCells(
    sheetXml: string,
    cellXfs: CellXf[],
    fonts: FontDef[],
    fills: FillDef[],
    borders: BorderDef[],
): Map<string, ParsedCellStyle> {
    const cellMap = new Map<string, ParsedCellStyle>();

    // Match <c> tags with r (reference) and s (style index) attributes
    const cellPattern = /<c\s[^>]*?\/?>/g;
    let m: RegExpExecArray | null;
    while ((m = cellPattern.exec(sheetXml)) !== null) {
        const ref = getAttr(m[0], "r");
        const styleIdx = getAttr(m[0], "s");
        if (!ref || styleIdx === null) continue;

        const xfIndex = parseInt(styleIdx, 10);
        const xf = cellXfs[xfIndex];
        if (!xf) continue;

        const parsed: ParsedCellStyle = {};
        let hasData = false;

        // Font properties (skip fontId 0 = default font, handled separately)
        if (xf.fontId !== 0) {
            const fontDef = fonts[xf.fontId];
            if (fontDef) {
                if (fontDef.color) {
                    parsed.color = fontDef.color;
                    hasData = true;
                }
                if (fontDef.bold) {
                    parsed.bold = true;
                    hasData = true;
                }
                if (fontDef.italic) {
                    parsed.italic = true;
                    hasData = true;
                }
                if (fontDef.size) {
                    parsed.size = fontDef.size;
                    hasData = true;
                }
                if (fontDef.name) {
                    parsed.name = fontDef.name;
                    hasData = true;
                }
                if (fontDef.underline) {
                    parsed.underline = true;
                    hasData = true;
                }
                if (fontDef.strike) {
                    parsed.strike = true;
                    hasData = true;
                }
            }
        }

        // Background fill color (skip white — solid white fill is visually
        // identical to no-fill in Excel but renders differently in Univer)
        const fillDef = fills[xf.fillId];
        if (fillDef?.color && fillDef.color.toUpperCase() !== "FFFFFF") {
            parsed.bg = fillDef.color;
            hasData = true;
        }

        // Borders
        if (xf.borderId > 0) {
            const borderDef = borders[xf.borderId];
            if (borderDef) {
                if (borderDef.left) {
                    parsed.borderLeft = borderDef.left;
                    hasData = true;
                }
                if (borderDef.right) {
                    parsed.borderRight = borderDef.right;
                    hasData = true;
                }
                if (borderDef.top) {
                    parsed.borderTop = borderDef.top;
                    hasData = true;
                }
                if (borderDef.bottom) {
                    parsed.borderBottom = borderDef.bottom;
                    hasData = true;
                }
            }
        }

        // Alignment
        if (xf.alignment) {
            if (xf.alignment.horizontal) {
                parsed.horizontalAlign = xf.alignment.horizontal;
                hasData = true;
            }
            if (xf.alignment.vertical) {
                parsed.verticalAlign = xf.alignment.vertical;
                hasData = true;
            }
            if (xf.alignment.wrapText) {
                parsed.wrapText = true;
                hasData = true;
            }
            if (xf.alignment.textRotation !== undefined) {
                parsed.textRotation = xf.alignment.textRotation;
                hasData = true;
            }
            if (xf.alignment.verticalText) {
                parsed.verticalText = true;
                hasData = true;
            }
        }

        if (hasData) cellMap.set(ref, parsed);
    }

    return cellMap;
}

// ── Freeze pane extraction ───────────────────────────────────────────────────

function parseSheetFreezePane(sheetXml: string): ParsedFreezePane | null {
    // Look for <pane> inside <sheetViews> → <sheetView>
    const paneMatch = sheetXml.match(/<pane\s[^>]*?\/?>/);
    if (!paneMatch) return null;

    const paneTag = paneMatch[0];
    const state = getAttr(paneTag, "state");
    if (state !== "frozen" && state !== "frozenSplit") return null;

    const xSplit = getAttr(paneTag, "xSplit");
    const ySplit = getAttr(paneTag, "ySplit");
    const x = xSplit ? parseInt(xSplit, 10) : 0;
    const y = ySplit ? parseInt(ySplit, 10) : 0;

    if (x === 0 && y === 0) return null;
    return { xSplit: x, ySplit: y };
}

// ── Main entry point ────────────────────────────────────────────────────────

export interface XlsxMetadata {
    fontStyles: XlsxCellStyleMap;
    freezePanes: Map<string, ParsedFreezePane>;
}

/**
 * Extracts font styles and freeze pane configuration from XLSX XML
 * in a single zip pass. SheetJS CE doesn't expose either of these,
 * so we parse the raw XML directly (web-worker-safe, no DOMParser).
 */
export async function extractXlsxMetadata(
    data: ArrayBuffer,
): Promise<XlsxMetadata> {
    const fontStyles: XlsxCellStyleMap = new Map();
    const freezePanes = new Map<string, ParsedFreezePane>();

    try {
        const zip = await JSZip.loadAsync(data);

        // Parse theme colors
        let themeColors: string[] = [];
        const themeFile = zip.file("xl/theme/theme1.xml");
        if (themeFile) {
            const themeXml = await themeFile.async("text");
            themeColors = parseThemeColors(themeXml);
        }

        // Parse styles
        const stylesFile = zip.file("xl/styles.xml");
        let fonts: FontDef[] = [];
        let cellXfs: CellXf[] = [];
        let fills: FillDef[] = [];
        let borders: BorderDef[] = [];

        if (stylesFile) {
            const stylesXml = await stylesFile.async("text");
            fonts = parseFonts(stylesXml, themeColors);
            fills = parseFills(stylesXml, themeColors);
            borders = parseBorders(stylesXml, themeColors);
            cellXfs = parseCellXfs(stylesXml);

            // Expose the default font (index 0) so the converter can set
            // it as the workbook/sheet default style in Univer.
            const defaultFontDef = fonts[0];
            if (defaultFontDef) {
                const df: ParsedCellStyle = {};
                if (defaultFontDef.size) df.size = defaultFontDef.size;
                if (defaultFontDef.name) df.name = defaultFontDef.name;
                if (defaultFontDef.bold) df.bold = true;
                if (defaultFontDef.italic) df.italic = true;
                if (defaultFontDef.color) df.color = defaultFontDef.color;
                if (Object.keys(df).length > 0) fontStyles.defaultFont = df;
            }
        }

        const hasStyleData = cellXfs.length > 0;

        // Resolve sheet paths
        const workbookFile = zip.file("xl/workbook.xml");
        const workbookRelsFile = zip.file("xl/_rels/workbook.xml.rels");
        if (!workbookFile || !workbookRelsFile) {
            return { fontStyles, freezePanes };
        }

        const workbookXml = await workbookFile.async("text");
        const workbookRelsXml = await workbookRelsFile.async("text");

        const sheets = parseWorkbookSheets(workbookXml);
        const rels = parseWorkbookRels(workbookRelsXml);

        // Parse each sheet for font styles and freeze panes
        for (const sheet of sheets) {
            const target = rels.get(sheet.rId);
            if (!target) continue;

            const sheetPath = target.startsWith("/")
                ? target.slice(1)
                : `xl/${target}`;

            const sheetFile = zip.file(sheetPath);
            if (!sheetFile) continue;

            const sheetXml = await sheetFile.async("text");

            if (hasStyleData) {
                const cellMap = parseSheetCells(
                    sheetXml,
                    cellXfs,
                    fonts,
                    fills,
                    borders,
                );
                if (cellMap.size > 0) {
                    fontStyles.set(sheet.name, cellMap);
                }
            }

            const freeze = parseSheetFreezePane(sheetXml);
            if (freeze) {
                freezePanes.set(sheet.name, freeze);
            }
        }
    } catch (e) {
        console.error("[xlsx-metadata] Error extracting XLSX metadata:", e);
    }

    return { fontStyles, freezePanes };
}
