import type {
    ICustomRange,
    ICustomTable,
    IDocumentData,
    IDocumentStyle,
    ILists,
    INumberUnit,
    IParagraph,
    IParagraphStyle,
    ISectionBreak,
    ITable,
    ITableCell,
    ITableCellBorder,
    ITableCellMargin,
    ITableColumn,
    ITableRow,
    ITextRun,
    ITextStyle,
    ITables,
} from "@univerjs/presets";
import {
    BaselineOffset,
    BooleanNumber,
    BulletAlignment,
    CustomRangeType,
    DashStyleType,
    DocumentFlavor,
    HorizontalAlign,
    ListGlyphType,
    NamedStyleType,
    NumberUnitType,
    ObjectRelativeFromH,
    ObjectRelativeFromV,
    TableAlignmentType,
    TableLayoutType,
    TableRowHeightRule,
    TableSizeType,
    TableTextWrapType,
    VerticalAlignmentType,
} from "@univerjs/presets";
import JSZip from "jszip";

/**
 * Direct DOCX (OOXML) → Univer IDocumentData converter.
 *
 * Parses the raw XML from word/document.xml and word/styles.xml to build
 * Univer's position-based document model without an intermediate HTML step.
 *
 * OOXML reference: https://learn.microsoft.com/en-us/openspecs/office_standards/ms-oi29500/
 * Key mappings:
 *   w:p        → paragraph (dataStream \r)
 *   w:r        → text run with inline styling
 *   w:rPr      → ITextStyle (bold, italic, underline, font, size, color)
 *   w:pPr      → IParagraphStyle (alignment, spacing, indentation)
 *   w:pStyle   → heading/list detection
 *   w:tbl      → styled text table with paragraph borders
 *   w:hyperlink → ICustomRange with HYPERLINK type
 *   w:br       → line/page break
 */

// DataStream table markers (verified from @univerjs/core runtime bundle)
const TABLE_START = "\x1A";
const TABLE_ROW_START = "\x1B";
const TABLE_CELL_START = "\x1C";
const TABLE_CELL_END = "\x1D";
const TABLE_ROW_END = "\x0E";
const TABLE_END = "\x0F";

// Map common DOCX fonts to web-safe equivalents
const FONT_MAP: Record<string, string> = {
    Calibri: "Calibri, Trebuchet MS, sans-serif",
    "Calibri Light": "Calibri, Trebuchet MS, sans-serif",
    Aptos: "Aptos, Calibri, Trebuchet MS, sans-serif",
    "Aptos Display": "Aptos, Calibri, Trebuchet MS, sans-serif",
    Cambria: "Cambria, Georgia, serif",
    "Times New Roman": "Times New Roman, Times, serif",
    Arial: "Arial, Helvetica, sans-serif",
    "Arial Black": "Arial Black, Arial, sans-serif",
    Verdana: "Verdana, Geneva, sans-serif",
    Tahoma: "Tahoma, Geneva, sans-serif",
    "Courier New": "Courier New, Courier, monospace",
    "Lucida Console": "Lucida Console, Monaco, monospace",
    Consolas: "Consolas, Courier New, monospace",
    Georgia: "Georgia, Times, serif",
    Palatino: "Palatino Linotype, Palatino, serif",
    "Book Antiqua": "Book Antiqua, Palatino, serif",
    Garamond: "Garamond, Georgia, serif",
    "Trebuchet MS": "Trebuchet MS, sans-serif",
    "Comic Sans MS": "Comic Sans MS, cursive",
    Impact: "Impact, sans-serif",
    "Century Gothic": "Century Gothic, sans-serif",
    "Segoe UI": "Segoe UI, Helvetica, Arial, sans-serif",
};

// Map OOXML paragraph style IDs to Univer NamedStyleType
const HEADING_STYLE_IDS: Record<string, NamedStyleType> = {
    Title: NamedStyleType.TITLE,
    Heading1: NamedStyleType.HEADING_1,
    Heading2: NamedStyleType.HEADING_2,
    Heading3: NamedStyleType.HEADING_3,
    Heading4: NamedStyleType.HEADING_4,
    Heading5: NamedStyleType.HEADING_5,
    // Common alternate IDs
    heading1: NamedStyleType.HEADING_1,
    heading2: NamedStyleType.HEADING_2,
    heading3: NamedStyleType.HEADING_3,
    heading4: NamedStyleType.HEADING_4,
    heading5: NamedStyleType.HEADING_5,
};

// Default font sizes for headings (points)
const HEADING_FONT_SIZES: Partial<Record<NamedStyleType, number>> = {
    [NamedStyleType.TITLE]: 28,
    [NamedStyleType.HEADING_1]: 24,
    [NamedStyleType.HEADING_2]: 20,
    [NamedStyleType.HEADING_3]: 16,
    [NamedStyleType.HEADING_4]: 14,
    [NamedStyleType.HEADING_5]: 12,
};

// Map OOXML alignment values to Univer HorizontalAlign
const ALIGNMENT_MAP: Record<string, HorizontalAlign> = {
    left: HorizontalAlign.LEFT,
    center: HorizontalAlign.CENTER,
    right: HorizontalAlign.RIGHT,
    both: HorizontalAlign.JUSTIFIED,
    justify: HorizontalAlign.JUSTIFIED,
};

interface BuilderState {
    dataStream: string;
    textRuns: ITextRun[];
    paragraphs: IParagraph[];
    customRanges: ICustomRange[];
    tables: ICustomTable[];
    tableSource: ITables;
    lists: ILists;
}

interface StyleInfo {
    headingLevel?: NamedStyleType;
    defaultRunProps?: ITextStyle;
}

const W_NS = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/**
 * Convert a DOCX ArrayBuffer to Univer IDocumentData by parsing OOXML directly.
 */
export async function docxToUniver(
    arrayBuffer: ArrayBuffer,
): Promise<IDocumentData> {
    const zip = await JSZip.loadAsync(arrayBuffer);

    // Parse main document
    const docXml = await zip.file("word/document.xml")?.async("string");
    if (!docXml) {
        throw new Error("Invalid DOCX: missing word/document.xml");
    }

    // Parse styles (optional)
    const stylesXml = await zip.file("word/styles.xml")?.async("string");
    const styleMap = stylesXml
        ? parseStyles(stylesXml)
        : new Map<string, StyleInfo>();

    // Parse relationships for hyperlinks
    const relsXml = await zip
        .file("word/_rels/document.xml.rels")
        ?.async("string");
    const relsMap = relsXml
        ? parseRelationships(relsXml)
        : new Map<string, string>();

    // Parse page/section properties from document XML
    const pageStyle = parseDocumentPageStyle(docXml);

    const parser = new DOMParser();
    const doc = parser.parseFromString(docXml, "application/xml");

    const body = doc.getElementsByTagNameNS(W_NS, "body")[0];
    if (!body) {
        throw new Error("Invalid DOCX: missing w:body element");
    }

    const state: BuilderState = {
        dataStream: "",
        textRuns: [],
        paragraphs: [],
        customRanges: [],
        tables: [],
        tableSource: {},
        lists: {},
    };

    // In MODERN flavor, Univer overrides pageSize/margins to:
    //   width = 595/0.75 = 793.33, marginLeft = marginRight = 50/0.75 = 66.67
    // So effective content width = 793.33 - 66.67*2 = 660
    const UNIVER_MODERN_CONTENT_WIDTH = 595 / 0.75 - 2 * (50 / 0.75);
    const contentWidth =
        pageStyle.documentFlavor === DocumentFlavor.MODERN
            ? UNIVER_MODERN_CONTENT_WIDTH
            : (pageStyle.pageSize?.width ?? 595.28) -
              (pageStyle.marginLeft ?? 72) -
              (pageStyle.marginRight ?? 72);
    processBody(body, state, styleMap, relsMap, contentWidth);

    // Ensure document ends with \r\n
    if (!state.dataStream.endsWith("\r\n")) {
        if (!state.dataStream.endsWith("\r")) {
            state.paragraphs.push({ startIndex: state.dataStream.length });
            state.dataStream += "\r";
        }
        state.dataStream += "\n";
    }

    const sectionBreaks: ISectionBreak[] = [
        { startIndex: state.dataStream.length - 1 },
    ];

    const result: IDocumentData = {
        id: `doc-${Date.now()}`,
        body: {
            dataStream: state.dataStream,
            textRuns: state.textRuns,
            paragraphs: state.paragraphs,
            sectionBreaks,
            customRanges:
                state.customRanges.length > 0 ? state.customRanges : undefined,
            tables: state.tables.length > 0 ? state.tables : undefined,
        },
        documentStyle: pageStyle,
    };

    if (Object.keys(state.tableSource).length > 0) {
        result.tableSource = state.tableSource;
    }

    if (Object.keys(state.lists).length > 0) {
        result.lists = state.lists;
    }

    return result;
}

function parseStyles(xml: string): Map<string, StyleInfo> {
    const map = new Map<string, StyleInfo>();
    const parser = new DOMParser();
    const doc = parser.parseFromString(xml, "application/xml");

    const styles = doc.getElementsByTagNameNS(W_NS, "style");
    for (const style of Array.from(styles)) {
        const styleId = style.getAttribute("w:styleId");
        if (!styleId) continue;

        const info: StyleInfo = {};

        // Check if it's a heading style
        const headingLevel = HEADING_STYLE_IDS[styleId];
        if (headingLevel !== undefined) {
            info.headingLevel = headingLevel;
        }

        // Check for basedOn reference to heading styles
        const basedOn = style.getElementsByTagNameNS(W_NS, "basedOn")[0];
        if (basedOn) {
            const basedOnId = basedOn.getAttribute("w:val");
            if (basedOnId && HEADING_STYLE_IDS[basedOnId] !== undefined) {
                info.headingLevel = HEADING_STYLE_IDS[basedOnId];
            }
        }

        // Extract default run properties from style
        const rPr = style.getElementsByTagNameNS(W_NS, "rPr")[0];
        if (rPr) {
            info.defaultRunProps = extractRunProperties(rPr);
        }

        map.set(styleId, info);
    }

    return map;
}

function parseRelationships(xml: string): Map<string, string> {
    const map = new Map<string, string>();
    const parser = new DOMParser();
    const doc = parser.parseFromString(xml, "application/xml");

    const rels = doc.getElementsByTagName("Relationship");
    for (const rel of Array.from(rels)) {
        const id = rel.getAttribute("Id");
        const target = rel.getAttribute("Target");
        const targetMode = rel.getAttribute("TargetMode");
        if (id && target && targetMode === "External") {
            map.set(id, target);
        }
    }

    return map;
}

function parseDocumentPageStyle(xml: string): IDocumentStyle {
    const parser = new DOMParser();
    const doc = parser.parseFromString(xml, "application/xml");

    // Default A4 dimensions in points
    let width = 595.28;
    let height = 841.89;
    let marginTop = 72;
    let marginBottom = 72;
    let marginLeft = 72;
    let marginRight = 72;

    // Look for w:sectPr (section properties) — usually the last child of w:body
    const sectPrs = doc.getElementsByTagNameNS(W_NS, "sectPr");
    if (sectPrs.length > 0) {
        const sectPr = sectPrs[sectPrs.length - 1]!;

        const pgSz = sectPr.getElementsByTagNameNS(W_NS, "pgSz")[0];
        if (pgSz) {
            // OOXML uses twips (1/20 of a point)
            const w = pgSz.getAttribute("w:w");
            const h = pgSz.getAttribute("w:h");
            if (w) width = parseInt(w, 10) / 20;
            if (h) height = parseInt(h, 10) / 20;
        }

        const pgMar = sectPr.getElementsByTagNameNS(W_NS, "pgMar")[0];
        if (pgMar) {
            const top = pgMar.getAttribute("w:top");
            const bottom = pgMar.getAttribute("w:bottom");
            const left = pgMar.getAttribute("w:left");
            const right = pgMar.getAttribute("w:right");
            if (top) marginTop = parseInt(top, 10) / 20;
            if (bottom) marginBottom = parseInt(bottom, 10) / 20;
            if (left) marginLeft = parseInt(left, 10) / 20;
            if (right) marginRight = parseInt(right, 10) / 20;
        }
    }

    return {
        pageSize: { width, height },
        documentFlavor: DocumentFlavor.MODERN,
        marginTop: Math.min(marginTop, 36),
        marginBottom: Math.min(marginBottom, 36),
        marginLeft: Math.min(marginLeft, 36),
        marginRight: Math.min(marginRight, 36),
    };
}

function processBody(
    body: Element,
    state: BuilderState,
    styleMap: Map<string, StyleInfo>,
    relsMap: Map<string, string>,
    contentWidth: number,
): void {
    for (const child of Array.from(body.children)) {
        const localName = child.localName;
        if (localName === "p") {
            processParagraph(child, state, styleMap, relsMap);
        } else if (localName === "tbl") {
            processTable(child, state, styleMap, relsMap, contentWidth);
        }
        // Skip w:sectPr and other non-content elements
    }
}

function processParagraph(
    pEl: Element,
    state: BuilderState,
    styleMap: Map<string, StyleInfo>,
    relsMap: Map<string, string>,
): void {
    const pPr = getFirstChild(pEl, "pPr");
    const paragraphStyle: IParagraphStyle = {};
    let isListItem = false;
    let listLevel = 0;
    let listNumId = "0";
    let resolvedStyleInfo: StyleInfo | undefined;

    if (pPr) {
        // Paragraph style reference
        const pStyle = getFirstChild(pPr, "pStyle");
        if (pStyle) {
            const styleId = pStyle.getAttribute("w:val");
            if (styleId) {
                resolvedStyleInfo = styleMap.get(styleId);
                if (resolvedStyleInfo?.headingLevel) {
                    paragraphStyle.namedStyleType =
                        resolvedStyleInfo.headingLevel;
                }
                // Also check directly against known heading IDs
                const headingLevel = HEADING_STYLE_IDS[styleId];
                if (headingLevel !== undefined) {
                    paragraphStyle.namedStyleType = headingLevel;
                }
            }
        }

        // Paragraph-level run properties (w:rPr inside w:pPr) — these apply to all runs
        const pRPr = getFirstChild(pPr, "rPr");
        if (pRPr) {
            const pRunProps = extractRunProperties(pRPr);
            if (Object.keys(pRunProps).length > 0) {
                if (!resolvedStyleInfo) {
                    resolvedStyleInfo = {};
                }
                // Merge: style defaults < paragraph rPr
                resolvedStyleInfo = {
                    ...resolvedStyleInfo,
                    defaultRunProps: {
                        ...(resolvedStyleInfo.defaultRunProps ?? {}),
                        ...pRunProps,
                    },
                };
            }
        }

        // Alignment
        const jc = getFirstChild(pPr, "jc");
        if (jc) {
            const align = jc.getAttribute("w:val");
            if (align && ALIGNMENT_MAP[align] !== undefined) {
                paragraphStyle.horizontalAlign = ALIGNMENT_MAP[align];
            }
        }

        // Indentation
        const ind = getFirstChild(pPr, "ind");
        if (ind) {
            const left = ind.getAttribute("w:left");
            const firstLine = ind.getAttribute("w:firstLine");
            const hanging = ind.getAttribute("w:hanging");
            if (left) {
                paragraphStyle.indentStart = {
                    v: parseInt(left, 10) / 20,
                    u: NumberUnitType.POINT,
                };
            }
            if (firstLine) {
                paragraphStyle.indentFirstLine = {
                    v: parseInt(firstLine, 10) / 20,
                    u: NumberUnitType.POINT,
                };
            }
            if (hanging) {
                paragraphStyle.hanging = {
                    v: parseInt(hanging, 10) / 20,
                    u: NumberUnitType.POINT,
                };
            }
        }

        // Spacing
        const spacing = getFirstChild(pPr, "spacing");
        if (spacing) {
            const before = spacing.getAttribute("w:before");
            const after = spacing.getAttribute("w:after");
            const line = spacing.getAttribute("w:line");
            if (before) {
                paragraphStyle.spaceAbove = {
                    v: parseInt(before, 10) / 20,
                    u: NumberUnitType.POINT,
                };
            }
            if (after) {
                paragraphStyle.spaceBelow = {
                    v: parseInt(after, 10) / 20,
                    u: NumberUnitType.POINT,
                };
            }
            if (line) {
                // Line spacing in twips; 240 twips = single spacing
                paragraphStyle.lineSpacing = parseInt(line, 10) / 240;
            }
        }

        // List detection (w:numPr)
        const numPr = getFirstChild(pPr, "numPr");
        if (numPr) {
            isListItem = true;
            const ilvl = getFirstChild(numPr, "ilvl");
            const numId = getFirstChild(numPr, "numId");
            if (ilvl) {
                listLevel = parseInt(ilvl.getAttribute("w:val") ?? "0", 10);
            }
            if (numId) {
                listNumId = numId.getAttribute("w:val") ?? "0";
            }
        }
    }

    // Process runs within this paragraph
    processRunsInParagraph(
        pEl,
        state,
        styleMap,
        relsMap,
        paragraphStyle,
        resolvedStyleInfo,
    );

    // Add paragraph marker
    const entry: IParagraph = {
        startIndex: state.dataStream.length,
    };

    if (Object.keys(paragraphStyle).length > 0) {
        entry.paragraphStyle = paragraphStyle;
    }

    if (isListItem) {
        const listKey = `list-${listNumId}`;

        // Register list definition so Univer can resolve the bullet
        if (!state.lists[listKey]) {
            state.lists[listKey] = {
                listType: listKey,
                nestingLevel: Array.from({ length: 9 }, (_, level) => ({
                    bulletAlignment: BulletAlignment.START,
                    glyphFormat: "%0",
                    startNumber: 1,
                    glyphType: ListGlyphType.BULLET,
                    glyphSymbol: "\u25CF",
                    paragraphProperties: {
                        indentStart: {
                            v: 18 * (level + 1),
                        },
                    },
                })),
            };
        }

        entry.bullet = {
            listType: listKey,
            listId: listKey,
            nestingLevel: listLevel,
            textStyle: {},
        };
    }

    state.paragraphs.push(entry);
    state.dataStream += "\r";
}

function processRunsInParagraph(
    pEl: Element,
    state: BuilderState,
    styleMap: Map<string, StyleInfo>,
    relsMap: Map<string, string>,
    paragraphStyle: IParagraphStyle,
    paragraphStyleInfo?: StyleInfo,
): void {
    // Build inherited run defaults from paragraph style
    const inheritedRunProps: ITextStyle = {};

    // Apply style-level run properties (e.g., heading color/font from styles.xml)
    if (paragraphStyleInfo?.defaultRunProps) {
        Object.assign(inheritedRunProps, paragraphStyleInfo.defaultRunProps);
    }

    // Apply heading defaults (font size + bold)
    const namedStyleType = paragraphStyle.namedStyleType;
    const headingFontSize = namedStyleType
        ? HEADING_FONT_SIZES[namedStyleType]
        : undefined;
    if (headingFontSize) {
        if (!inheritedRunProps.fs) inheritedRunProps.fs = headingFontSize;
        if (!inheritedRunProps.bl) inheritedRunProps.bl = BooleanNumber.TRUE;
    }

    for (const child of Array.from(pEl.children)) {
        const localName = child.localName;

        if (localName === "r") {
            processRun(child, state, styleMap, inheritedRunProps);
        } else if (localName === "hyperlink") {
            processHyperlink(
                child,
                state,
                styleMap,
                relsMap,
                inheritedRunProps,
            );
        }
        // Skip w:pPr, w:bookmarkStart, etc.
    }
}

function processRun(
    rEl: Element,
    state: BuilderState,
    styleMap: Map<string, StyleInfo>,
    inheritedRunProps: ITextStyle,
): void {
    const rPr = getFirstChild(rEl, "rPr");
    const explicitStyle = rPr ? extractRunProperties(rPr) : {};

    // Build final style: inherited defaults < rStyle defaults < explicit properties
    const runStyle: ITextStyle = { ...inheritedRunProps };

    // Merge rStyle reference defaults
    if (rPr) {
        const rStyle = getFirstChild(rPr, "rStyle");
        if (rStyle) {
            const styleId = rStyle.getAttribute("w:val");
            if (styleId) {
                const info = styleMap.get(styleId);
                if (info?.defaultRunProps) {
                    Object.assign(runStyle, info.defaultRunProps);
                }
            }
        }
    }

    // Explicit properties always win
    Object.assign(runStyle, explicitStyle);

    for (const child of Array.from(rEl.children)) {
        const localName = child.localName;

        if (localName === "t") {
            // Text content
            const text = child.textContent ?? "";
            if (text.length === 0) continue;

            const st = state.dataStream.length;
            state.dataStream += text;
            const ed = state.dataStream.length;

            if (Object.keys(runStyle).length > 0) {
                state.textRuns.push({ st, ed, ts: { ...runStyle } });
            }
        } else if (localName === "br") {
            // Break element
            const type = child.getAttribute("w:type");
            if (type === "page") {
                // Page break: \f in Univer
                state.dataStream += "\f";
            } else {
                // Line break within paragraph — just add newline text
                state.dataStream += "\n";
            }
        } else if (localName === "tab") {
            state.dataStream += "\t";
        } else if (localName === "cr") {
            // Carriage return
            state.dataStream += "\n";
        }
    }
}

function processHyperlink(
    hlEl: Element,
    state: BuilderState,
    styleMap: Map<string, StyleInfo>,
    relsMap: Map<string, string>,
    inheritedRunProps: ITextStyle,
): void {
    const rId = hlEl.getAttribute("r:id");
    const url = rId ? relsMap.get(rId) : undefined;

    const rangeStart = state.dataStream.length;

    // Add custom range start marker
    state.dataStream += "\x1F";
    const contentStart = state.dataStream.length;

    // Process child runs
    for (const child of Array.from(hlEl.children)) {
        if (child.localName === "r") {
            processRun(child, state, styleMap, inheritedRunProps);
        }
    }

    const contentEnd = state.dataStream.length;

    // Add custom range end marker
    state.dataStream += "\x1E";

    if (url) {
        state.customRanges.push({
            startIndex: rangeStart,
            endIndex: contentEnd,
            rangeId: `link-${state.customRanges.length}`,
            rangeType: CustomRangeType.HYPERLINK,
            properties: { url },
        });

        // Style the hyperlink text (blue + underline)
        if (contentStart < contentEnd) {
            state.textRuns.push({
                st: contentStart,
                ed: contentEnd,
                ts: {
                    cl: { rgb: "#0563C1" },
                    ul: { s: BooleanNumber.TRUE },
                },
            });
        }
    }
}

// OOXML border style → Univer DashStyleType
const BORDER_DASH_MAP: Record<string, DashStyleType> = {
    single: DashStyleType.SOLID,
    thick: DashStyleType.SOLID,
    double: DashStyleType.SOLID,
    dotted: DashStyleType.DOT,
    dashed: DashStyleType.DASH,
    dashSmallGap: DashStyleType.DASH,
    dotDash: DashStyleType.DASH,
    dotDotDash: DashStyleType.DASH,
    nil: DashStyleType.DASH_STYLE_UNSPECIFIED,
    none: DashStyleType.DASH_STYLE_UNSPECIFIED,
};

// OOXML table alignment → Univer TableAlignmentType
const TABLE_ALIGN_MAP: Record<string, TableAlignmentType> = {
    start: TableAlignmentType.START,
    left: TableAlignmentType.START,
    center: TableAlignmentType.CENTER,
    end: TableAlignmentType.END,
    right: TableAlignmentType.END,
};

// OOXML vertical alignment → Univer VerticalAlignmentType
const VALIGN_MAP: Record<string, VerticalAlignmentType> = {
    top: VerticalAlignmentType.TOP,
    center: VerticalAlignmentType.CENTER,
    bottom: VerticalAlignmentType.BOTTOM,
};

function parseBorderElement(
    borderEl: Element | null,
): ITableCellBorder | undefined {
    if (!borderEl) return undefined;
    const val = borderEl.getAttribute("w:val");
    if (!val || val === "nil" || val === "none") return undefined;

    const color = borderEl.getAttribute("w:color");
    const sz = borderEl.getAttribute("w:sz");
    // w:sz is in eighth-points (1/8 pt)
    const widthPt = sz ? parseInt(sz, 10) / 8 : 1;

    return {
        color: { rgb: color && color !== "auto" ? `#${color}` : "#000000" },
        width: { v: widthPt },
        dashStyle: BORDER_DASH_MAP[val] ?? DashStyleType.SOLID,
    };
}

function parseCellMargin(
    marginEl: Element | null,
): ITableCellMargin | undefined {
    if (!marginEl) return undefined;
    const top = getFirstChild(marginEl, "top");
    const bottom = getFirstChild(marginEl, "bottom");
    const start =
        getFirstChild(marginEl, "start") ?? getFirstChild(marginEl, "left");
    const end =
        getFirstChild(marginEl, "end") ?? getFirstChild(marginEl, "right");

    const parse = (el: Element | null): INumberUnit => {
        if (!el) return { v: 0 };
        const w = el.getAttribute("w:w");
        return { v: w ? parseInt(w, 10) / 20 : 0 };
    };

    return {
        top: parse(top),
        bottom: parse(bottom),
        start: parse(start),
        end: parse(end),
    };
}

function processTable(
    tblEl: Element,
    state: BuilderState,
    styleMap: Map<string, StyleInfo>,
    relsMap: Map<string, string>,
    contentWidth: number,
): void {
    const tableId = `table-${Object.keys(state.tableSource).length}`;
    const rows = getDirectChildren(tblEl, "tr");
    const tableRows: ITableRow[] = [];
    const columnWidths: number[] = [];

    // Parse table properties
    const tblPr = getFirstChild(tblEl, "tblPr");

    // Parse table grid for column widths
    const tblGrid = getFirstChild(tblEl, "tblGrid");
    if (tblGrid) {
        for (const col of getDirectChildren(tblGrid, "gridCol")) {
            const w = col.getAttribute("w:w");
            columnWidths.push(w ? parseInt(w, 10) / 20 : 100);
        }
    }

    // Parse table-level borders (used as defaults for cells)
    const tblBorders = tblPr ? getFirstChild(tblPr, "tblBorders") : null;
    const defaultBorders = tblBorders
        ? {
              top: parseBorderElement(getFirstChild(tblBorders, "top")),
              bottom: parseBorderElement(getFirstChild(tblBorders, "bottom")),
              left: parseBorderElement(getFirstChild(tblBorders, "left")),
              right: parseBorderElement(getFirstChild(tblBorders, "right")),
              insideH: parseBorderElement(getFirstChild(tblBorders, "insideH")),
              insideV: parseBorderElement(getFirstChild(tblBorders, "insideV")),
          }
        : null;

    // Parse table-level cell margin defaults
    const tblCellMar = tblPr ? getFirstChild(tblPr, "tblCellMar") : null;
    const defaultCellMargin = parseCellMargin(tblCellMar);

    // Parse table alignment
    const tblJc = tblPr ? getFirstChild(tblPr, "jc") : null;
    const tableAlign =
        TABLE_ALIGN_MAP[tblJc?.getAttribute("w:val") ?? ""] ??
        TableAlignmentType.START;

    // Parse explicit table width
    const tblW = tblPr ? getFirstChild(tblPr, "tblW") : null;
    const explicitTableWidth = tblW?.getAttribute("w:w");

    const tableStartIndex = state.dataStream.length;
    state.dataStream += TABLE_START;

    const totalRows = rows.length;
    for (let rowIdx = 0; rowIdx < totalRows; rowIdx++) {
        const row = rows[rowIdx]!;
        state.dataStream += TABLE_ROW_START;

        const cells = getDirectChildren(row, "tc");
        const totalCells = cells.length;
        const rowCells: ITableCell[] = [];

        for (let cellIdx = 0; cellIdx < totalCells; cellIdx++) {
            const cell = cells[cellIdx]!;
            state.dataStream += TABLE_CELL_START;

            const cellDef: ITableCell = {};

            // Parse cell properties
            const tcPr = getFirstChild(cell, "tcPr");
            if (tcPr) {
                // Cell width
                const tcW = getFirstChild(tcPr, "tcW");
                if (tcW) {
                    const w = tcW.getAttribute("w:w");
                    if (w) {
                        cellDef.size = {
                            type: TableSizeType.SPECIFIED,
                            width: { v: parseInt(w, 10) / 20 },
                        };
                    }
                }

                // Column span
                const gridSpan = getFirstChild(tcPr, "gridSpan");
                if (gridSpan) {
                    const span = parseInt(
                        gridSpan.getAttribute("w:val") ?? "1",
                        10,
                    );
                    if (span > 1) cellDef.columnSpan = span;
                }

                // Row span (vMerge)
                const vMerge = getFirstChild(tcPr, "vMerge");
                if (vMerge) {
                    const mergeVal = vMerge.getAttribute("w:val");
                    if (mergeVal === "restart") {
                        // Count how many rows this spans
                        let spanCount = 1;
                        for (let ri = rowIdx + 1; ri < totalRows; ri++) {
                            const nextCells = getDirectChildren(
                                rows[ri]!,
                                "tc",
                            );
                            if (cellIdx < nextCells.length) {
                                const nextTcPr = getFirstChild(
                                    nextCells[cellIdx]!,
                                    "tcPr",
                                );
                                const nextVMerge = nextTcPr
                                    ? getFirstChild(nextTcPr, "vMerge")
                                    : null;
                                if (
                                    nextVMerge &&
                                    nextVMerge.getAttribute("w:val") !==
                                        "restart"
                                ) {
                                    spanCount++;
                                } else {
                                    break;
                                }
                            }
                        }
                        if (spanCount > 1) cellDef.rowSpan = spanCount;
                    }
                }

                // Cell borders (override table-level defaults)
                const tcBorders = getFirstChild(tcPr, "tcBorders");
                const cellBorderTop =
                    parseBorderElement(
                        tcBorders ? getFirstChild(tcBorders, "top") : null,
                    ) ??
                    (rowIdx === 0
                        ? defaultBorders?.top
                        : defaultBorders?.insideH);
                const cellBorderBottom =
                    parseBorderElement(
                        tcBorders ? getFirstChild(tcBorders, "bottom") : null,
                    ) ??
                    (rowIdx === totalRows - 1
                        ? defaultBorders?.bottom
                        : defaultBorders?.insideH);
                const cellBorderLeft =
                    parseBorderElement(
                        tcBorders ? getFirstChild(tcBorders, "left") : null,
                    ) ??
                    (cellIdx === 0
                        ? defaultBorders?.left
                        : defaultBorders?.insideV);
                const cellBorderRight =
                    parseBorderElement(
                        tcBorders ? getFirstChild(tcBorders, "right") : null,
                    ) ??
                    (cellIdx === totalCells - 1
                        ? defaultBorders?.right
                        : defaultBorders?.insideV);

                if (cellBorderTop) cellDef.borderTop = cellBorderTop;
                if (cellBorderBottom) cellDef.borderBottom = cellBorderBottom;
                if (cellBorderLeft) cellDef.borderLeft = cellBorderLeft;
                if (cellBorderRight) cellDef.borderRight = cellBorderRight;

                // Cell background color
                const shd = getFirstChild(tcPr, "shd");
                if (shd) {
                    const fill = shd.getAttribute("w:fill");
                    if (fill && fill !== "auto") {
                        cellDef.backgroundColor = { rgb: `#${fill}` };
                    }
                }

                // Cell vertical alignment
                const vAlignEl = getFirstChild(tcPr, "vAlign");
                if (vAlignEl) {
                    const va = VALIGN_MAP[vAlignEl.getAttribute("w:val") ?? ""];
                    if (va !== undefined) cellDef.vAlign = va;
                }

                // Cell margins (override table defaults)
                const tcMar = getFirstChild(tcPr, "tcMar");
                const margin = parseCellMargin(tcMar) ?? defaultCellMargin;
                if (margin) cellDef.margin = margin;
            } else if (defaultCellMargin) {
                cellDef.margin = defaultCellMargin;
            }

            // Apply table-level default borders if cell has no tcPr
            if (!tcPr && defaultBorders) {
                const bt =
                    rowIdx === 0 ? defaultBorders.top : defaultBorders.insideH;
                const bb =
                    rowIdx === totalRows - 1
                        ? defaultBorders.bottom
                        : defaultBorders.insideH;
                const bl =
                    cellIdx === 0
                        ? defaultBorders.left
                        : defaultBorders.insideV;
                const br =
                    cellIdx === totalCells - 1
                        ? defaultBorders.right
                        : defaultBorders.insideV;
                if (bt) cellDef.borderTop = bt;
                if (bb) cellDef.borderBottom = bb;
                if (bl) cellDef.borderLeft = bl;
                if (br) cellDef.borderRight = br;
            }

            // Process cell content — each cell must have at least one paragraph (\r)
            const cellParagraphs = getDirectChildren(cell, "p");
            if (cellParagraphs.length > 0) {
                for (const p of cellParagraphs) {
                    processParagraph(p, state, styleMap, relsMap);
                }
            } else {
                // Empty cell still needs a paragraph marker
                state.paragraphs.push({ startIndex: state.dataStream.length });
                state.dataStream += "\r";
            }

            rowCells.push(cellDef);
            // Each cell must end with a section break before CELL_END
            // so that the parser can attach paragraphs to the cell node
            state.dataStream += "\n";
            state.dataStream += TABLE_CELL_END;
        }

        // Parse row height
        const trPr = getFirstChild(row, "trPr");
        const trHeight = trPr ? getFirstChild(trPr, "trHeight") : null;
        const rowHeightVal = trHeight?.getAttribute("w:val");
        const rowHeight = rowHeightVal ? parseInt(rowHeightVal, 10) / 20 : 20;
        const hRuleVal = trHeight?.getAttribute("w:hRule");
        const hRule: TableRowHeightRule =
            hRuleVal === "exact"
                ? TableRowHeightRule.EXACT
                : hRuleVal === "atLeast"
                  ? TableRowHeightRule.AT_LEAST
                  : TableRowHeightRule.AUTO;

        tableRows.push({
            tableCells: rowCells,
            trHeight: { val: { v: rowHeight }, hRule },
            isFirstRow: rowIdx === 0 ? BooleanNumber.TRUE : BooleanNumber.FALSE,
        });

        state.dataStream += TABLE_ROW_END;
    }

    state.dataStream += TABLE_END;
    const tableEndIndex = state.dataStream.length;

    // Build column definitions, scaling to fill content width
    const numCols =
        columnWidths.length > 0
            ? columnWidths.length
            : rows[0]
              ? getDirectChildren(rows[0], "tc").length
              : 1;

    // Determine target table width
    const tblWType = tblW?.getAttribute("w:type");
    let targetWidth: number;
    if (explicitTableWidth && tblWType === "dxa") {
        targetWidth = parseInt(explicitTableWidth, 10) / 20;
    } else if (explicitTableWidth && tblWType === "pct") {
        // Percentage of page width (value is in 1/50ths of a percent)
        targetWidth = (parseInt(explicitTableWidth, 10) / 5000) * contentWidth;
    } else {
        // Auto or unspecified — use content width
        targetWidth = contentWidth;
    }

    const rawTotal = columnWidths.reduce((s, w) => s + w, 0);
    const scale = rawTotal > 0 ? targetWidth / rawTotal : 1;

    const tableColumns: ITableColumn[] = [];
    for (let i = 0; i < numCols; i++) {
        const raw = columnWidths[i] ?? targetWidth / numCols;
        tableColumns.push({
            size: { type: TableSizeType.SPECIFIED, width: { v: raw * scale } },
        });
    }

    const totalWidth = targetWidth;

    // Scale cell widths to match column scaling
    if (scale !== 1) {
        for (const row of tableRows) {
            for (const cell of row.tableCells) {
                if (cell.size) {
                    cell.size.width.v *= scale;
                }
            }
        }
    }

    const tableDef: ITable = {
        tableId,
        tableRows,
        tableColumns,
        align: tableAlign,
        indent: { v: 0 },
        textWrap: TableTextWrapType.NONE,
        position: {
            positionH: { relativeFrom: ObjectRelativeFromH.PAGE },
            positionV: { relativeFrom: ObjectRelativeFromV.PAGE },
        },
        dist: { distT: 0, distB: 0, distL: 0, distR: 0 },
        size: { type: TableSizeType.SPECIFIED, width: { v: totalWidth } },
        layout: TableLayoutType.FIXED,
    };

    if (defaultCellMargin) {
        tableDef.cellMargin = defaultCellMargin;
    }

    // Parse table indent
    const tblInd = tblPr ? getFirstChild(tblPr, "tblInd") : null;
    if (tblInd) {
        const indW = tblInd.getAttribute("w:w");
        if (indW) tableDef.indent = { v: parseInt(indW, 10) / 20 };
    }

    state.tableSource[tableId] = tableDef;

    // The Univer parser registers tables on the \r AFTER TABLE_END.
    // It matches body.tables entries where endIndex === table_node.endIndex + 1.
    // Add a paragraph after the table for proper registration.
    state.paragraphs.push({ startIndex: state.dataStream.length });
    state.dataStream += "\r";

    state.tables.push({
        startIndex: tableStartIndex,
        endIndex: tableEndIndex,
        tableId,
    });
}

function getDirectChildren(parent: Element, localName: string): Element[] {
    const result: Element[] = [];
    for (const child of Array.from(parent.children)) {
        if (child.localName === localName) {
            result.push(child);
        }
    }
    return result;
}

/**
 * Extract Univer ITextStyle properties from a w:rPr element.
 */
function extractRunProperties(rPr: Element): ITextStyle {
    const style: ITextStyle = {};

    // Bold: w:b or w:bCs
    if (hasToggleProperty(rPr, "b")) {
        style.bl = BooleanNumber.TRUE;
    }

    // Italic: w:i or w:iCs
    if (hasToggleProperty(rPr, "i")) {
        style.it = BooleanNumber.TRUE;
    }

    // Underline: w:u
    const u = getFirstChild(rPr, "u");
    if (u) {
        const val = u.getAttribute("w:val");
        if (val && val !== "none") {
            style.ul = { s: BooleanNumber.TRUE };
        }
    }

    // Strikethrough: w:strike
    if (hasToggleProperty(rPr, "strike")) {
        style.st = { s: BooleanNumber.TRUE };
    }

    // Font size: w:sz (in half-points)
    const sz = getFirstChild(rPr, "sz");
    if (sz) {
        const val = sz.getAttribute("w:val");
        if (val) {
            style.fs = parseInt(val, 10) / 2; // Convert half-points to points
        }
    }

    // Font family: w:rFonts
    const rFonts = getFirstChild(rPr, "rFonts");
    if (rFonts) {
        const ff =
            rFonts.getAttribute("w:ascii") ??
            rFonts.getAttribute("w:hAnsi") ??
            rFonts.getAttribute("w:cs");
        if (ff) {
            style.ff = FONT_MAP[ff] ?? ff;
        }
    }

    // Font color: w:color
    const color = getFirstChild(rPr, "color");
    if (color) {
        const val = color.getAttribute("w:val");
        if (val && val !== "auto") {
            style.cl = { rgb: `#${val}` };
        }
    }

    // Superscript/subscript: w:vertAlign
    const vertAlign = getFirstChild(rPr, "vertAlign");
    if (vertAlign) {
        const val = vertAlign.getAttribute("w:val");
        if (val === "superscript") {
            style.va = BaselineOffset.SUPERSCRIPT;
        } else if (val === "subscript") {
            style.va = BaselineOffset.SUBSCRIPT;
        }
    }

    // Highlight/background color: w:highlight or w:shd
    const highlight = getFirstChild(rPr, "highlight");
    if (highlight) {
        const val = highlight.getAttribute("w:val");
        if (val && val !== "none") {
            const highlightColor = HIGHLIGHT_COLORS[val];
            if (highlightColor) {
                style.bg = { rgb: highlightColor };
            }
        }
    }

    return style;
}

// Common highlight color names → hex values
const HIGHLIGHT_COLORS: Record<string, string> = {
    yellow: "#FFFF00",
    green: "#00FF00",
    cyan: "#00FFFF",
    magenta: "#FF00FF",
    blue: "#0000FF",
    red: "#FF0000",
    darkBlue: "#00008B",
    darkCyan: "#008B8B",
    darkGreen: "#006400",
    darkMagenta: "#8B008B",
    darkRed: "#8B0000",
    darkYellow: "#808000",
    darkGray: "#A9A9A9",
    lightGray: "#D3D3D3",
    black: "#000000",
    white: "#FFFFFF",
};

/**
 * Check if a toggle property (like w:b, w:i, w:strike) is enabled.
 * In OOXML, presence of the element means true unless w:val="false" or w:val="0".
 */
function hasToggleProperty(parent: Element, name: string): boolean {
    const el = getFirstChild(parent, name);
    if (!el) return false;
    const val = el.getAttribute("w:val");
    return val !== "false" && val !== "0";
}

/**
 * Get the first child element with the given local name in the w: namespace.
 */
function getFirstChild(parent: Element, localName: string): Element | null {
    for (const child of Array.from(parent.children)) {
        if (child.localName === localName) {
            return child;
        }
    }
    return null;
}
