//! A [`DocumentParser`](crate::DocumentParser) that reads a workbook as a
//! workbook: sheets, rows, and cells, each addressable by its A1 reference.
//!
//! Gated behind the `parse-spreadsheet` feature. Registered ahead of
//! [`LiteParseOfficeParser`](crate::LiteParseOfficeParser), which otherwise
//! claims the same types and converts them through LibreOffice to PDF. That
//! detour produces perfectly good prose and throws away the only thing a
//! spreadsheet citation is written in: once a workbook is a page of rendered
//! glyphs, `Sheet 'Q4 Results', B5:D10` is not recoverable from it. Reading the
//! file natively keeps the grid, so every run of canonical text carries the cell
//! it came from.
//!
//! Canonical text is Markdown — one ATX heading per sheet, then one line per
//! row with the cells separated by pipes. The heading matters beyond
//! readability: the chunker partitions Markdown at headings, so a chunk never
//! straddles two sheets, which is what makes "this passage is a range on one
//! sheet" true rather than approximately true.
//!
//! `calamine` does the file reading. It is pure Rust and needs nothing installed
//! at runtime, so unlike the LibreOffice path this parser produces the same
//! result on every machine.

use std::io::Cursor;

use async_trait::async_trait;
use calamine::{Data, DataType, Range, Reader};
use openwave_core::{CellAddress, SourceLocation, SourceRegion};

use crate::document::ByteSpan;
use crate::error::{Result, RetrievalError};
use crate::parse::{DocumentParser, ParsedDocument};

/// The spreadsheet media types this parser claims: Excel in its OOXML, binary,
/// and legacy forms, and the OpenDocument equivalent. Word and PowerPoint are
/// untouched and stay with the Office parser; CSV/TSV are `text/*` and stay with
/// the plain-text parser, which already leaves their text addressable verbatim.
const SPREADSHEET_MEDIA_TYPES: &[&str] = &[
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.ms-excel.sheet.macroenabled.12",
    "application/vnd.ms-excel.sheet.binary.macroenabled.12",
    "application/vnd.ms-excel",
    "application/vnd.oasis.opendocument.spreadsheet",
];

/// Stable identity of this parser's canonical-text and cell-map behavior. Bump
/// the trailing version whenever either changes: stored regions mean what the
/// parser that wrote them meant, and they are only rebuilt when this moves.
const SPREADSHEET_FINGERPRINT: &str = "calamine:v0.36:sheets:markdown:cells:v1";

/// Most cells one workbook contributes to the canonical text and its cell map.
///
/// A cell map is stored with the document and read back on every search that
/// touches it, so an export with a million populated cells must not become a
/// million regions. Past the limit the workbook is still read — it keeps the
/// cells up to the limit, and the sheets past it are summarized rather than
/// transcribed, which is a truthful floor instead of a silently truncated one.
const MAX_CELLS: usize = 100_000;

/// Longest rendering of one cell's value. A cell holding a pasted essay is one
/// cell, and the passage a reader wants from it is at its start.
const MAX_CELL_CHARS: usize = 512;

/// Longest sheet name recorded, in characters.
///
/// The bound evidence enforces is in bytes, and a character can be four of
/// them, so this is a quarter of it — comfortably past what any spreadsheet
/// application lets a sheet be called.
const MAX_SHEET_NAME_CHARS: usize = openwave_core::EvidenceLocation::MAX_SHEET_NAME_BYTES / 4;

/// Reads workbooks natively, preserving sheet and cell addresses.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpreadsheetParser;

impl SpreadsheetParser {
    /// Construct the parser.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// The base media type, lowercased and stripped of any `; charset=…` suffix.
    fn base_media_type(media_type: &str) -> String {
        media_type
            .split(';')
            .next()
            .unwrap_or(media_type)
            .trim()
            .to_ascii_lowercase()
    }
}

#[async_trait]
impl DocumentParser for SpreadsheetParser {
    fn fingerprint_for(&self, media_type: &str) -> Option<String> {
        self.supports(media_type)
            .then(|| SPREADSHEET_FINGERPRINT.to_string())
    }

    fn supports(&self, media_type: &str) -> bool {
        SPREADSHEET_MEDIA_TYPES.contains(&Self::base_media_type(media_type).as_str())
    }

    /// The grid is rendered as Markdown, which is what makes the chunker cut at
    /// sheet boundaries rather than mid-grid.
    fn canonical_media_type(&self, _media_type: &str) -> String {
        "text/markdown".to_string()
    }

    async fn parse(&self, raw: &[u8], media_type: &str) -> Result<ParsedDocument> {
        if !self.supports(media_type) {
            return Err(RetrievalError::parse(format!(
                "SpreadsheetParser does not support media type `{media_type}`"
            )));
        }
        read_workbook(raw)
    }
}

/// Render every sheet of `raw`, with the cell each run of text came from.
fn read_workbook(raw: &[u8]) -> Result<ParsedDocument> {
    let mut workbook = calamine::open_workbook_auto_from_rs(Cursor::new(raw.to_vec()))
        .map_err(|error| RetrievalError::parse(format!("could not read the workbook: {error}")))?;

    let mut rendered = Rendering::default();
    for (index, name) in workbook.sheet_names().into_iter().enumerate() {
        // A sheet that will not open is named and skipped rather than failing
        // the workbook: the other sheets are still worth indexing, and a reader
        // opening the source sees the sheet is there.
        let range = workbook.worksheet_range(&name).ok();
        let index = i32::try_from(index).map_err(|_| {
            RetrievalError::parse("the workbook has more sheets than can be addressed")
        })?;
        rendered.sheet(index, &name, range.as_ref());
    }
    Ok(ParsedDocument::from_text(rendered.text).with_source_regions(rendered.regions))
}

/// The canonical text being built, and the cell each of its runs belongs to.
#[derive(Default)]
struct Rendering {
    text: String,
    regions: Vec<SourceRegion>,
    cells: usize,
}

impl Rendering {
    /// Append one sheet: its heading, then a line per row.
    fn sheet(&mut self, index: i32, name: &str, range: Option<&Range<Data>>) {
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        // The heading is the sheet's name as a reader sees it in the tab bar.
        // A name is required on every cell recorded from this sheet, so an
        // unusable one is replaced here rather than left to fail validation and
        // cost the whole workbook its index.
        let name = sheet_name(name, index);
        self.text.push_str(&format!("## {name}\n\n"));

        let Some(range) = range.filter(|range| range.get_size() != (0, 0)) else {
            self.text.push_str("_(empty sheet)_\n");
            return;
        };
        let (first_row, first_column) = range.start().unwrap_or((0, 0));
        for (row_offset, row) in range.rows().enumerate() {
            if self.cells >= MAX_CELLS {
                self.text
                    .push_str("_(the rest of this workbook was not transcribed)_\n");
                return;
            }
            self.row(index, &name, row, first_row, first_column, row_offset);
        }
    }

    /// Append one row as `| value | value |`, recording each nonempty cell.
    ///
    /// The pipes and the spaces around them are gaps between regions, which is
    /// exactly what they are: separators this parser inserted, belonging to no
    /// cell. A row whose cells are all empty is skipped, so a sheet with data
    /// far down it does not render as a thousand blank lines.
    fn row(
        &mut self,
        sheet_index: i32,
        sheet_name: &str,
        row: &[Data],
        first_row: u32,
        first_column: u32,
        row_offset: usize,
    ) {
        if row.iter().all(DataType::is_empty) {
            return;
        }
        let Ok(row_offset) = u32::try_from(row_offset) else {
            return;
        };
        let mut line = String::from("|");
        let mut pending: Vec<(ByteSpan, String)> = Vec::new();
        for (column_offset, value) in row.iter().enumerate() {
            let Ok(column_offset) = u32::try_from(column_offset) else {
                break;
            };
            line.push(' ');
            let rendered = one_line(&render_cell(value));
            // A cell past what A1 notation addresses keeps its text and loses
            // its address: no real sheet is that wide or tall, and recording a
            // reference nothing downstream accepts would cost the workbook its
            // whole index rather than that one cell its position.
            let address = CellAddress {
                column: first_column.saturating_add(column_offset),
                row: first_row.saturating_add(row_offset),
            }
            .to_a1();
            if let (false, Some(address)) = (rendered.is_empty(), address) {
                let start = self.text.len() + line.len();
                pending.push((ByteSpan::new(start, start + rendered.len()), address));
            }
            line.push_str(&rendered);
            line.push_str(" |");
        }
        line.push('\n');
        self.text.push_str(&line);
        self.cells += pending.len();
        self.regions
            .extend(pending.into_iter().map(|(span, start_cell)| SourceRegion {
                span,
                location: SourceLocation::SpreadsheetCells {
                    sheet_index,
                    sheet_name: sheet_name.to_owned(),
                    start_cell,
                    end_cell: None,
                },
            }));
    }
}

/// The name a sheet is recorded and shown under.
///
/// Every cell recorded from a sheet carries its name, and a name that is empty,
/// past the bound evidence enforces, or spread over several lines would fail
/// validation for the whole document. A sheet with no usable name of its own is
/// named by its position instead, the way a spreadsheet application does.
fn sheet_name(name: &str, index: i32) -> String {
    let flattened = one_line(name);
    let bounded = match flattened.char_indices().nth(MAX_SHEET_NAME_CHARS) {
        Some((cut, _)) => flattened[..cut].trim_end().to_owned(),
        None => flattened,
    };
    if bounded.is_empty() {
        format!("Sheet {}", index.saturating_add(1))
    } else {
        bounded
    }
}

/// One cell's value as text, as a reader of the sheet would see it.
///
/// Dates are the case worth spelling out: a workbook stores them as a serial
/// number, and `45657` in a search result is worse than useless. They are
/// rendered as calendar dates, and a value that is a time of day only is
/// rendered as one.
fn render_cell(value: &Data) -> String {
    match value {
        Data::Empty => String::new(),
        Data::DateTime(_) | Data::DateTimeIso(_) => value
            .as_datetime()
            .map(|moment| {
                let stamp = moment.format("%Y-%m-%d %H:%M:%S").to_string();
                // A date with no time of day is a date, and the midnight a
                // workbook stores under it is an artifact of the format.
                stamp
                    .strip_suffix(" 00:00:00")
                    .map_or_else(|| stamp.clone(), str::to_owned)
            })
            .unwrap_or_else(|| value.to_string()),
        other => other.to_string(),
    }
}

/// Flatten a value onto one line and bound it, so a cell stays a cell.
///
/// Line breaks inside a cell would otherwise make the row layout lie about
/// which values are on which row, and a cell holding a pasted document would
/// make one region cover a page of text.
fn one_line(value: &str) -> String {
    let flattened: String = value
        .chars()
        .take(MAX_CELL_CHARS)
        .map(|character| {
            if character.is_control() || character == '\u{a0}' {
                ' '
            } else {
                character
            }
        })
        .collect();
    // Cells are separated by pipes, so a pipe inside one would read as a cell
    // boundary that is not there.
    flattened.replace('|', "/").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    use openwave_core::EvidenceLocation;

    const XLSX_MEDIA_TYPE: &str =
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

    /// The cell each run of `parsed`'s text was recorded under, paired with the
    /// text itself.
    fn located(parsed: &ParsedDocument) -> Vec<(String, String, &str)> {
        parsed
            .source_regions
            .iter()
            .map(|region| {
                let cells = region
                    .location
                    .spreadsheet_cells()
                    .expect("a workbook records cells");
                (
                    cells.sheet_name.to_owned(),
                    cells.start_cell.to_owned(),
                    &parsed.text[region.span.start..region.span.end],
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn a_workbook_parses_to_text_whose_runs_are_addressable_cells() {
        let workbook = super::test_fixtures::build_xlsx(&[
            (
                "Q4 Results",
                &[
                    &["Region", "Revenue", "Margin"],
                    &["North", "1204.5", "0.31"],
                    &["South", "980", "0.22"],
                ],
            ),
            ("Notes", &[&["Prepared by finance"]]),
        ]);

        let parsed = SpreadsheetParser::new()
            .parse(&workbook, XLSX_MEDIA_TYPE)
            .await
            .expect("a well-formed workbook parses");

        assert!(
            parsed.text.starts_with("## Q4 Results\n"),
            "each sheet opens its own Markdown section: {:?}",
            parsed.text
        );
        assert!(parsed.text.contains("## Notes\n"));
        assert_eq!(
            located(&parsed),
            vec![
                ("Q4 Results".into(), "A1".into(), "Region"),
                ("Q4 Results".into(), "B1".into(), "Revenue"),
                ("Q4 Results".into(), "C1".into(), "Margin"),
                ("Q4 Results".into(), "A2".into(), "North"),
                ("Q4 Results".into(), "B2".into(), "1204.5"),
                ("Q4 Results".into(), "C2".into(), "0.31"),
                ("Q4 Results".into(), "A3".into(), "South"),
                ("Q4 Results".into(), "B3".into(), "980"),
                ("Q4 Results".into(), "C3".into(), "0.22"),
                ("Notes".into(), "A1".into(), "Prepared by finance"),
            ]
        );
        openwave_core::validate_source_regions(&parsed.text, &parsed.source_regions)
            .expect("a workbook's cell map is valid against its own text");
    }

    #[tokio::test]
    async fn workbook_regions_resolve_to_the_range_they_cover() {
        let workbook = super::test_fixtures::build_xlsx(&[(
            "Q4 Results",
            &[
                &["Region", "Revenue", "Margin"],
                &["North", "1204.5", "0.31"],
                &["South", "980", "0.22"],
            ],
        )]);
        let parsed = SpreadsheetParser::new()
            .parse(&workbook, XLSX_MEDIA_TYPE)
            .await
            .unwrap();
        let located =
            EvidenceLocation::for_source_regions(Vec::new(), parsed.source_regions.clone());
        assert_eq!(
            located,
            EvidenceLocation::SpreadsheetCellRange {
                start_cell: "A1".into(),
                end_cell: Some("C3".into()),
                sheet_index: 0,
                sheet_name: "Q4 Results".into(),
            },
            "the whole grid is one range on its sheet"
        );
        assert!(located.is_well_formed());
    }

    #[tokio::test]
    async fn a_cell_value_stays_one_cell_however_it_was_typed() {
        let workbook = super::test_fixtures::build_xlsx(&[(
            "Sheet1",
            &[&["two\nlines", "a | pipe", ""], &["", "", "trailing"]],
        )]);
        let parsed = SpreadsheetParser::new()
            .parse(&workbook, XLSX_MEDIA_TYPE)
            .await
            .unwrap();

        assert_eq!(
            located(&parsed),
            vec![
                ("Sheet1".into(), "A1".into(), "two lines"),
                ("Sheet1".into(), "B1".into(), "a / pipe"),
                ("Sheet1".into(), "C2".into(), "trailing"),
            ]
        );
        assert_eq!(
            parsed.text.lines().filter(|line| !line.is_empty()).count(),
            3
        );
    }
    #[tokio::test]
    async fn bytes_that_are_not_a_workbook_fail_rather_than_indexing_as_prose() {
        let error = SpreadsheetParser::new()
            .parse(b"this is not a workbook", XLSX_MEDIA_TYPE)
            .await
            .expect_err("unreadable bytes are a parse failure");
        assert!(error.to_string().contains("could not read the workbook"));
    }

    #[test]
    fn claims_spreadsheets_and_nothing_else_office_owns() {
        let parser = SpreadsheetParser::new();
        assert!(parser.supports(XLSX_MEDIA_TYPE));
        assert!(parser.supports("application/vnd.oasis.opendocument.spreadsheet"));
        assert!(parser.supports("APPLICATION/VND.MS-EXCEL; charset=binary"));
        // Word, PowerPoint, and delimited text keep the parsers they had.
        assert!(!parser
            .supports("application/vnd.openxmlformats-officedocument.wordprocessingml.document"));
        assert!(!parser
            .supports("application/vnd.openxmlformats-officedocument.presentationml.presentation"));
        assert!(!parser.supports("text/csv"));
        assert!(!parser.supports("application/pdf"));
    }
}

/// Building a real `.xlsx` in memory, so the parser is driven by a file the way
/// an upload would drive it rather than by a hand-built `Range`.
#[cfg(test)]
mod test_fixtures {
    /// The smallest workbook Excel and `calamine` both accept, carrying `sheets`
    /// as inline strings. Entries are stored uncompressed so this needs no zip
    /// crate.
    pub(super) fn build_xlsx(sheets: &[(&str, &[&[&str]])]) -> Vec<u8> {
        let content_types = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>{}\
</Types>",
            (1..=sheets.len())
                .map(|n| format!(
                    "<Override PartName=\"/xl/worksheets/sheet{n}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"
                ))
                .collect::<String>()
        );
        let package_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>\
</Relationships>";
        let workbook = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>{}</sheets></workbook>",
            sheets
                .iter()
                .enumerate()
                .map(|(index, (name, _))| format!(
                    "<sheet name=\"{name}\" sheetId=\"{n}\" r:id=\"rId{n}\"/>",
                    n = index + 1
                ))
                .collect::<String>()
        );
        let workbook_rels = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{}</Relationships>",
            (1..=sheets.len())
                .map(|n| format!(
                    "<Relationship Id=\"rId{n}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{n}.xml\"/>"
                ))
                .collect::<String>()
        );

        let mut entries: Vec<(String, Vec<u8>)> = vec![
            ("[Content_Types].xml".into(), content_types.into_bytes()),
            ("_rels/.rels".into(), package_rels.as_bytes().to_vec()),
            ("xl/workbook.xml".into(), workbook.into_bytes()),
            (
                "xl/_rels/workbook.xml.rels".into(),
                workbook_rels.into_bytes(),
            ),
        ];
        for (index, (_, rows)) in sheets.iter().enumerate() {
            entries.push((
                format!("xl/worksheets/sheet{}.xml", index + 1),
                worksheet(rows).into_bytes(),
            ));
        }
        stored_zip(&entries)
    }

    /// One worksheet part, with every value written as an inline string so the
    /// fixture needs no shared-strings table.
    fn worksheet(rows: &[&[&str]]) -> String {
        let body = rows
            .iter()
            .enumerate()
            .map(|(row_index, row)| {
                let cells = row
                    .iter()
                    .enumerate()
                    .filter(|(_, value)| !value.is_empty())
                    .map(|(column_index, value)| {
                        let column = char::from(b'A' + u8::try_from(column_index).expect("< 26"));
                        format!(
                            "<c r=\"{column}{row}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",
                            escape(value),
                            row = row_index + 1
                        )
                    })
                    .collect::<String>();
                format!("<row r=\"{}\">{cells}</row>", row_index + 1)
            })
            .collect::<String>();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>{body}</sheetData></worksheet>"
        )
    }

    fn escape(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\n', "&#10;")
    }

    /// A ZIP archive with stored (uncompressed) entries — minimal, but valid
    /// enough for a reader to open.
    fn stored_zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
        fn crc32(data: &[u8]) -> u32 {
            let mut crc: u32 = 0xFFFF_FFFF;
            for &byte in data {
                crc ^= u32::from(byte);
                for _ in 0..8 {
                    let mask = (crc & 1).wrapping_neg();
                    crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
                }
            }
            !crc
        }

        let mut out = Vec::new();
        let mut central = Vec::new();
        let mut offsets = Vec::new();
        for (name, data) in entries {
            let name_bytes = name.as_bytes();
            let crc = crc32(data);
            offsets.push(u32::try_from(out.len()).expect("fixture archives are small"));
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // local file header
            out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            out.extend_from_slice(&0u16.to_le_bytes()); // flags
            out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
            out.extend_from_slice(&0u16.to_le_bytes()); // mod time
            out.extend_from_slice(&0u16.to_le_bytes()); // mod date
            out.extend_from_slice(&crc.to_le_bytes());
            let size = u32::try_from(data.len()).expect("fixture parts are small");
            out.extend_from_slice(&size.to_le_bytes()); // compressed size
            out.extend_from_slice(&size.to_le_bytes()); // uncompressed size
            out.extend_from_slice(
                &u16::try_from(name_bytes.len())
                    .expect("fixture names are short")
                    .to_le_bytes(),
            );
            out.extend_from_slice(&0u16.to_le_bytes()); // extra len
            out.extend_from_slice(name_bytes);
            out.extend_from_slice(data);
        }
        for (index, (name, data)) in entries.iter().enumerate() {
            let name_bytes = name.as_bytes();
            let crc = crc32(data);
            let size = u32::try_from(data.len()).expect("fixture parts are small");
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // central header
            central.extend_from_slice(&20u16.to_le_bytes()); // version made by
            central.extend_from_slice(&20u16.to_le_bytes()); // version needed
            central.extend_from_slice(&0u16.to_le_bytes()); // flags
            central.extend_from_slice(&0u16.to_le_bytes()); // method: stored
            central.extend_from_slice(&0u16.to_le_bytes()); // mod time
            central.extend_from_slice(&0u16.to_le_bytes()); // mod date
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&size.to_le_bytes());
            central.extend_from_slice(&size.to_le_bytes());
            central.extend_from_slice(
                &u16::try_from(name_bytes.len())
                    .expect("fixture names are short")
                    .to_le_bytes(),
            );
            central.extend_from_slice(&0u16.to_le_bytes()); // extra len
            central.extend_from_slice(&0u16.to_le_bytes()); // comment len
            central.extend_from_slice(&0u16.to_le_bytes()); // disk number
            central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            central.extend_from_slice(&offsets[index].to_le_bytes());
            central.extend_from_slice(name_bytes);
        }
        let directory_offset = u32::try_from(out.len()).expect("fixture archives are small");
        let directory_size = u32::try_from(central.len()).expect("fixture archives are small");
        let count = u16::try_from(entries.len()).expect("fixtures hold few parts");
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes()); // end of central directory
        out.extend_from_slice(&[0u8; 4]); // disk numbers
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&directory_size.to_le_bytes());
        out.extend_from_slice(&directory_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out
    }
}
