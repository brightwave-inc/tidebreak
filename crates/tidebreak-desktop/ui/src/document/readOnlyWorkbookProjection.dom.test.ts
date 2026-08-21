// @vitest-environment jsdom

import JSZip from "jszip";
import { beforeEach, describe, expect, it, vi } from "vitest";

const formulaEngine = vi.hoisted(() => ({
  evaluateUncachedFormulasWithDuke: vi.fn<
    (
      source: ArrayBuffer,
      cells: ReadonlyArray<{ address: string; sheetIndex: number }>,
    ) => Promise<Record<
      string,
      { type: string; value: boolean | number | string }
    > | null>
  >(async () => null),
}));

vi.mock("./readOnlyWorkbookFormulaEngine", () => ({
  evaluateUncachedFormulasWithDuke:
    formulaEngine.evaluateUncachedFormulasWithDuke,
  formulaValueKey: (sheetIndex: number, address: string) =>
    `${sheetIndex}:${address}`,
}));

import { projectWorkbookForReadOnlyDisplay } from "./readOnlyWorkbookProjection";

describe("projectWorkbookForReadOnlyDisplay", () => {
  beforeEach(() => {
    formulaEngine.evaluateUncachedFormulasWithDuke.mockReset();
    formulaEngine.evaluateUncachedFormulasWithDuke.mockResolvedValue(null);
  });

  it("renders cached values while retaining original formulas for inspection", async () => {
    const zip = new JSZip();
    zip.file(
      "xl/workbook.xml",
      `<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Model" sheetId="1" r:id="rId1"/></sheets></workbook>`,
    );
    zip.file(
      "xl/_rels/workbook.xml.rels",
      `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Target="worksheets/sheet1.xml" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"/></Relationships>`,
    );
    zip.file(
      "xl/worksheets/sheet1.xml",
      `<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><f>SUM(B1:B3)</f><v>42</v></c><c r="B1"><f>NOW()</f></c></row></sheetData></worksheet>`,
    );

    const source = await zip.generateAsync({ type: "arraybuffer" });
    const projection = await projectWorkbookForReadOnlyDisplay(source);
    const projectedZip = await JSZip.loadAsync(projection.data);
    const sheetXml = await projectedZip
      .file("xl/worksheets/sheet1.xml")!
      .async("string");
    const document = new DOMParser().parseFromString(
      sheetXml,
      "application/xml",
    );
    const cells = Array.from(document.getElementsByTagNameNS("*", "c"));

    expect(projection.formulasBySheet).toEqual({
      0: { A1: "=SUM(B1:B3)", B1: "=NOW()" },
    });
    expect(cells[0]?.getElementsByTagNameNS("*", "f")).toHaveLength(0);
    expect(cells[0]?.getElementsByTagNameNS("*", "v")[0]?.textContent).toBe(
      "42",
    );
    expect(cells[1]?.getElementsByTagNameNS("*", "f")).toHaveLength(1);
  });

  it("does not treat an empty cached value as a formula result", async () => {
    const zip = new JSZip();
    zip.file(
      "xl/workbook.xml",
      `<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Model" sheetId="1" r:id="rId1"/></sheets></workbook>`,
    );
    zip.file(
      "xl/_rels/workbook.xml.rels",
      `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Target="worksheets/sheet1.xml" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"/></Relationships>`,
    );
    zip.file(
      "xl/worksheets/sheet1.xml",
      `<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="2"><c r="B2"><f>Assumptions!B6/4</f><v/></c></row></sheetData></worksheet>`,
    );

    const source = await zip.generateAsync({ type: "arraybuffer" });
    const projection = await projectWorkbookForReadOnlyDisplay(source);
    const projectedZip = await JSZip.loadAsync(projection.data);
    const sheetXml = await projectedZip
      .file("xl/worksheets/sheet1.xml")!
      .async("string");
    const document = new DOMParser().parseFromString(
      sheetXml,
      "application/xml",
    );
    const cell = document.getElementsByTagNameNS("*", "c")[0];

    expect(projection.formulasBySheet).toEqual({
      0: { B2: "=Assumptions!B6/4" },
    });
    expect(cell?.getElementsByTagNameNS("*", "f")).toHaveLength(1);
    expect(cell?.getElementsByTagNameNS("*", "v")).toHaveLength(0);
  });

  it("bakes calculated results into formula cells that have no cached value", async () => {
    formulaEngine.evaluateUncachedFormulasWithDuke.mockResolvedValue({
      "0:B2": { type: "number", value: 25.6 },
    });

    const zip = new JSZip();
    zip.file(
      "xl/workbook.xml",
      `<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="FCF Model" sheetId="1" r:id="rId1"/></sheets></workbook>`,
    );
    zip.file(
      "xl/_rels/workbook.xml.rels",
      `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Target="worksheets/sheet1.xml" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"/></Relationships>`,
    );
    zip.file(
      "xl/worksheets/sheet1.xml",
      `<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="2"><c r="B2"><f>Assumptions!$B$6/4</f></c></row></sheetData></worksheet>`,
    );

    const source = await zip.generateAsync({ type: "arraybuffer" });
    const projection = await projectWorkbookForReadOnlyDisplay(source);
    const projectedZip = await JSZip.loadAsync(projection.data);
    const sheetXml = await projectedZip
      .file("xl/worksheets/sheet1.xml")!
      .async("string");
    const document = new DOMParser().parseFromString(
      sheetXml,
      "application/xml",
    );
    const cell = document.getElementsByTagNameNS("*", "c")[0];

    expect(formulaEngine.evaluateUncachedFormulasWithDuke).toHaveBeenCalledWith(
      source,
      [{ address: "B2", sheetIndex: 0 }],
    );
    expect(projection.formulasBySheet).toEqual({
      0: { B2: "=Assumptions!$B$6/4" },
    });
    expect(cell?.getElementsByTagNameNS("*", "f")).toHaveLength(0);
    expect(cell?.getElementsByTagNameNS("*", "v")[0]?.textContent).toBe("25.6");
  });

  it("preserves border indices by expanding empty OOXML border records", async () => {
    const zip = new JSZip();
    zip.file(
      "xl/styles.xml",
      `<?xml version="1.0"?><x:styleSheet xmlns:x="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><x:borders count="2"><x:border/><x:border><x:bottom style="thin"/></x:border></x:borders></x:styleSheet>`,
    );

    const source = await zip.generateAsync({ type: "arraybuffer" });
    const projection = await projectWorkbookForReadOnlyDisplay(source);
    const projectedZip = await JSZip.loadAsync(projection.data);
    const stylesXml = await projectedZip.file("xl/styles.xml")!.async("string");
    const document = new DOMParser().parseFromString(
      stylesXml,
      "application/xml",
    );
    const borders = Array.from(document.getElementsByTagNameNS("*", "border"));

    expect(
      Array.from(borders[0]!.children, (child) => child.localName),
    ).toEqual(["left", "right", "top", "bottom", "diagonal"]);
    expect(
      Array.from(borders[1]!.children, (child) => child.localName),
    ).toEqual(["bottom"]);
  });

  it("makes inherited workbook-theme chart series colors explicit", async () => {
    const zip = new JSZip();
    zip.file(
      "xl/theme/theme1.xml",
      `<?xml version="1.0"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements><a:clrScheme><a:accent1><a:srgbClr val="156082"/></a:accent1><a:accent2><a:srgbClr val="E97132"/></a:accent2><a:accent3><a:srgbClr val="196B24"/></a:accent3></a:clrScheme></a:themeElements></a:theme>`,
    );
    zip.file(
      "xl/drawings/charts/chart1.xml",
      `<?xml version="1.0"?><c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"><c:chart><c:plotArea><c:lineChart><c:varyColors val="0"/><c:ser><c:idx val="0"/><c:order val="0"/><c:marker/></c:ser><c:ser><c:idx val="1"/><c:order val="1"/><c:marker/></c:ser><c:ser><c:idx val="2"/><c:order val="2"/><c:marker/></c:ser></c:lineChart></c:plotArea></c:chart></c:chartSpace>`,
    );

    const source = await zip.generateAsync({ type: "arraybuffer" });
    const projection = await projectWorkbookForReadOnlyDisplay(source);
    const projectedZip = await JSZip.loadAsync(projection.data);
    const chartXml = await projectedZip
      .file("xl/drawings/charts/chart1.xml")!
      .async("string");
    const document = new DOMParser().parseFromString(
      chartXml,
      "application/xml",
    );
    const series = Array.from(document.getElementsByTagNameNS("*", "ser"));

    expect(
      series.map((item) =>
        item
          .getElementsByTagNameNS("*", "spPr")[0]
          ?.getElementsByTagNameNS("*", "srgbClr")[0]
          ?.getAttribute("val"),
      ),
    ).toEqual(["156082", "E97132", "196B24"]);
  });

  it("projects data bars and color scales for the canvas renderer", async () => {
    const zip = new JSZip();
    zip.file(
      "xl/workbook.xml",
      `<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Dashboard" sheetId="1" r:id="rId1"/></sheets></workbook>`,
    );
    zip.file(
      "xl/_rels/workbook.xml.rels",
      `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Target="worksheets/sheet1.xml" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"/></Relationships>`,
    );
    zip.file(
      "xl/worksheets/sheet1.xml",
      `<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="B1"><v>10</v></c><c r="D1"><v>0</v></c></row><row r="2"><c r="B2"><v>15</v></c><c r="D2"><v>0.5</v></c></row><row r="3"><c r="B3"><v>20</v></c><c r="D3"><v>1</v></c></row></sheetData><conditionalFormatting sqref="D1:D3"><cfRule type="dataBar" priority="1"><dataBar><cfvo type="min"/><cfvo type="max"/><color rgb="FF2979FF"/></dataBar></cfRule></conditionalFormatting><conditionalFormatting sqref="B1:B3"><cfRule type="colorScale" priority="2"><colorScale><cfvo type="min"/><cfvo type="percentile" val="50"/><cfvo type="max"/><color rgb="FFE7FAF6"/><color rgb="FFFFF3C4"/><color rgb="FFFDEBEC"/></colorScale></cfRule></conditionalFormatting></worksheet>`,
    );

    const source = await zip.generateAsync({ type: "arraybuffer" });
    const projection = await projectWorkbookForReadOnlyDisplay(source);

    expect(projection.conditionalStylesBySheet[0]).toMatchObject({
      B1: { backgroundColor: "rgb(231, 250, 246)" },
      B2: { backgroundColor: "rgb(255, 243, 196)" },
      B3: { backgroundColor: "rgb(253, 235, 236)" },
      D1: { dataBar: { color: "#2979ff", widthPercent: 0 } },
      D2: { dataBar: { color: "#2979ff", widthPercent: 50 } },
      D3: { dataBar: { color: "#2979ff", widthPercent: 100 } },
    });
  });

  it("projects data bars from baked formula results, not empty cached values", async () => {
    formulaEngine.evaluateUncachedFormulasWithDuke.mockResolvedValue({
      "0:D1": { type: "number", value: 0 },
      "0:D2": { type: "number", value: 0.5 },
      "0:D3": { type: "number", value: 1 },
    });

    const zip = new JSZip();
    zip.file(
      "xl/workbook.xml",
      `<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Dashboard" sheetId="1" r:id="rId1"/></sheets></workbook>`,
    );
    zip.file(
      "xl/_rels/workbook.xml.rels",
      `<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Target="worksheets/sheet1.xml" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"/></Relationships>`,
    );
    zip.file(
      "xl/worksheets/sheet1.xml",
      `<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="D1"><f>A1</f><v/></c></row><row r="2"><c r="D2"><f>A2</f><v/></c></row><row r="3"><c r="D3"><f>A3</f><v/></c></row></sheetData><conditionalFormatting sqref="D1:D3"><cfRule type="dataBar" priority="1"><dataBar><cfvo type="min"/><cfvo type="max"/><color rgb="FF2979FF"/></dataBar></cfRule></conditionalFormatting></worksheet>`,
    );

    const source = await zip.generateAsync({ type: "arraybuffer" });
    const projection = await projectWorkbookForReadOnlyDisplay(source);

    expect(projection.conditionalStylesBySheet[0]).toMatchObject({
      D1: { dataBar: { color: "#2979ff", widthPercent: 0 } },
      D2: { dataBar: { color: "#2979ff", widthPercent: 50 } },
      D3: { dataBar: { color: "#2979ff", widthPercent: 100 } },
    });
  });

  it("reads Excel packaging parts that start with a UTF-8 BOM", async () => {
    const zip = new JSZip();
    zip.file(
      "xl/workbook.xml",
      `<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Model" sheetId="1" r:id="rId1"/></sheets></workbook>`,
    );
    zip.file(
      "xl/_rels/workbook.xml.rels",
      `\uFEFF<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Target="worksheets/sheet1.xml" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet"/></Relationships>`,
    );
    zip.file(
      "xl/worksheets/sheet1.xml",
      `<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><f>SUM(B1:B3)</f><v>42</v></c></row></sheetData></worksheet>`,
    );

    const NativeParser = globalThis.DOMParser;
    // jsdom accepts a leading U+FEFF; the desktop webview does not.
    class WebKitLikeParser {
      parseFromString(xml: string, type: DOMParserSupportedType) {
        if (xml.startsWith("\uFEFF")) {
          return new NativeParser().parseFromString(
            "<parsererror>XML declaration allowed only at the start of the document</parsererror>",
            "application/xml",
          );
        }
        return new NativeParser().parseFromString(xml, type);
      }
    }
    globalThis.DOMParser = WebKitLikeParser as typeof DOMParser;
    try {
      const source = await zip.generateAsync({ type: "arraybuffer" });
      const projection = await projectWorkbookForReadOnlyDisplay(source);
      expect(projection.formulasBySheet).toEqual({ 0: { A1: "=SUM(B1:B3)" } });
    } finally {
      globalThis.DOMParser = NativeParser;
    }
  });

  it("keeps the original workbook when a package part cannot be parsed", async () => {
    const zip = new JSZip();
    zip.file("xl/styles.xml", "not-xml");
    const source = await zip.generateAsync({ type: "arraybuffer" });

    const projection = await projectWorkbookForReadOnlyDisplay(source);

    expect(projection.data).toBe(source);
    expect(projection.formulasBySheet).toEqual({});
    expect(projection.conditionalStylesBySheet).toEqual({});
  });
});
