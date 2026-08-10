//! Trusted native decision about what an imported document actually is.
//!
//! Everything downstream keys off the media type: which parser runs, whether
//! the source ends up searchable, and how the renderer labels it. Nothing that
//! chose the file gets to decide that answer. A picker selection and an
//! agent-named path under a connected folder both arrive here as bytes plus an
//! untrusted filename, and the bytes win wherever they identify themselves.

use std::path::Path;

/// Media type used when nothing identifies the bytes. The server accepts it and
/// stores the source; it simply will not be searchable.
pub const OPAQUE_MEDIA_TYPE: &str = "application/octet-stream";

/// Bytes inspected when deciding a media type.
///
/// Every format that matters here is identified by a magic number within the
/// first few hundred bytes, except the Zip-container formats, whose central
/// directory `infer` reads from the local file header near the start. Bounding
/// the window keeps sniffing cheap for a multi-megabyte document.
pub const SNIFF_WINDOW_BYTES: usize = 8_192;

/// Text formats that carry no magic number, so only the name can distinguish
/// them. All of these reach the same plain-text parser, so a wrong guess here
/// changes the stored label rather than whether the source is searchable.
const TEXT_EXTENSIONS: &[(&str, &str)] = &[
    ("md", "text/markdown"),
    ("markdown", "text/markdown"),
    ("txt", "text/plain"),
    ("text", "text/plain"),
    ("log", "text/plain"),
    ("csv", "text/csv"),
    ("tsv", "text/tab-separated-values"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("json", "application/json"),
    ("xml", "application/xml"),
    ("yaml", "application/yaml"),
    ("yml", "application/yaml"),
];

/// Office formats that are Zip containers. `infer` reads their content types
/// out of the archive, but a container it cannot classify is reported as plain
/// Zip; for those the extension is the only remaining signal.
const ZIP_CONTAINER_EXTENSIONS: &[(&str, &str)] = &[
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("odt", "application/vnd.oasis.opendocument.text"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet"),
    ("odp", "application/vnd.oasis.opendocument.presentation"),
];

/// Decide the media type of imported bytes without trusting whoever named them.
///
/// Content is authoritative wherever it identifies itself, so a `.pdf` that is
/// really a Zip archive is never handed to the PDF parser, and a document with
/// a meaningless extension is still parsed as what it is. The filename is
/// consulted only where content genuinely cannot answer: text formats have no
/// magic number at all, and a Zip container that `infer` cannot classify could
/// be any of the Office formats built on Zip.
///
/// `infer`'s own text heuristics are deliberately ignored. They match on
/// leading tags, so a Markdown file that opens with an HTML comment sniffs as
/// HTML. The extension is the better signal there, and both answers reach the
/// same parser regardless.
pub fn sniff_media_type(bytes: &[u8], file_name: Option<&str>) -> String {
    let extension = extension_of(file_name);
    let window = &bytes[..bytes.len().min(SNIFF_WINDOW_BYTES)];

    if let Some(detected) = infer::get(window) {
        match detected.matcher_type() {
            // Heuristic, and weaker than the filename. Fall through.
            infer::MatcherType::Text => {}
            _ if is_unclassified_zip(detected.mime_type()) => {
                if let Some(refined) = lookup(ZIP_CONTAINER_EXTENSIONS, extension.as_deref()) {
                    return refined.to_owned();
                }
                return detected.mime_type().to_owned();
            }
            _ => return detected.mime_type().to_owned(),
        }
    }

    if let Some(text) = lookup(TEXT_EXTENSIONS, extension.as_deref()) {
        return text.to_owned();
    }
    // Nothing named it and nothing in it identified it. Decodable bytes are
    // still worth indexing; the rest are stored as opaque.
    if std::str::from_utf8(window).is_ok() {
        "text/plain".to_owned()
    } else {
        OPAQUE_MEDIA_TYPE.to_owned()
    }
}

/// Media type for a file chosen from the local picker, where the absolute path
/// is available but only its final component may inform the answer.
pub fn sniff_media_type_for_path(bytes: &[u8], path: &Path) -> String {
    sniff_media_type(bytes, path.file_name().and_then(|name| name.to_str()))
}

fn extension_of(file_name: Option<&str>) -> Option<String> {
    Path::new(file_name?)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
}

fn lookup(table: &[(&str, &'static str)], extension: Option<&str>) -> Option<&'static str> {
    let extension = extension?;
    table
        .iter()
        .find(|(candidate, _)| *candidate == extension)
        .map(|(_, media_type)| *media_type)
}

fn is_unclassified_zip(mime_type: &str) -> bool {
    matches!(mime_type, "application/zip" | "application/epub+zip")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smallest byte sequences that the real matchers accept, so these tests
    /// exercise sniffing rather than a stubbed table.
    const PDF: &[u8] = b"%PDF-1.7\n1 0 obj\n";
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

    /// A Zip whose first entry lives under `word/`, which is how the OOXML
    /// matcher recognizes a Word document.
    fn docx() -> Vec<u8> {
        zip_header("word/document.xml")
    }

    /// A Zip local file header, whose filename field begins at offset 0x1E —
    /// exactly where the OOXML matcher reads it.
    fn zip_header(name: &str) -> Vec<u8> {
        let mut bytes = vec![0x50, 0x4b, 0x03, 0x04];
        bytes.extend_from_slice(&[0x14, 0x00, 0x06, 0x00, 0x08, 0x00, 0x00, 0x00, 0x21, 0x00]);
        bytes.extend_from_slice(&[0u8; 12]);
        bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes
    }

    #[test]
    fn content_overrides_a_misleading_or_missing_extension() {
        // The whole point: a name cannot route bytes to the wrong parser.
        assert_eq!(
            sniff_media_type(PDF, Some("invoice.txt")),
            "application/pdf"
        );
        assert_eq!(sniff_media_type(PDF, Some("invoice")), "application/pdf");
        assert_eq!(sniff_media_type(PDF, None), "application/pdf");
        assert_eq!(sniff_media_type(PNG, Some("diagram.pdf")), "image/png");
        // ...and a real document with a meaningless name is still identified.
        assert_eq!(
            sniff_media_type(&docx(), Some("attachment.bin")),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
    }

    #[test]
    fn text_formats_fall_back_to_the_name_they_were_given() {
        let markdown = b"# Notes\n\nA plain document.\n";
        assert_eq!(
            sniff_media_type(markdown, Some("notes.md")),
            "text/markdown"
        );
        assert_eq!(
            sniff_media_type(markdown, Some("notes.MD")),
            "text/markdown"
        );
        assert_eq!(
            sniff_media_type(b"a,b,c\n1,2,3\n", Some("t.csv")),
            "text/csv"
        );
        // infer's text matchers would call this HTML; the extension is better,
        // and both answers reach the same parser anyway.
        assert_eq!(
            sniff_media_type(b"<!-- a comment -->\n# Notes\n", Some("notes.md")),
            "text/markdown"
        );
    }

    #[test]
    fn unidentifiable_bytes_are_text_only_when_they_decode() {
        assert_eq!(sniff_media_type(b"just some prose", None), "text/plain");
        assert_eq!(
            sniff_media_type(b"just some prose", Some("notes.unknown")),
            "text/plain"
        );
        assert_eq!(
            sniff_media_type(&[0xff, 0xfe, 0x00, 0x80], Some("blob.unknown")),
            OPAQUE_MEDIA_TYPE
        );
        assert_eq!(sniff_media_type(&[], None), "text/plain");
    }

    #[test]
    fn an_unclassified_zip_defers_to_the_extension_only_among_office_types() {
        let plain_zip = zip_header("readme.txt");
        assert_eq!(
            sniff_media_type(&plain_zip, Some("report.docx")),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        // An archive that claims no Office extension stays an archive rather
        // than being guessed into a parser that would fail on it.
        assert_eq!(
            sniff_media_type(&plain_zip, Some("bundle.zip")),
            "application/zip"
        );
        assert_eq!(sniff_media_type(&plain_zip, None), "application/zip");
    }

    #[test]
    fn sniffing_reads_only_a_bounded_prefix() {
        // A magic number past the window is not searched for, so a huge file
        // never costs more than the window to classify.
        let mut buried = vec![b' '; SNIFF_WINDOW_BYTES];
        buried.extend_from_slice(PDF);
        assert_eq!(sniff_media_type(&buried, None), "text/plain");
        assert_eq!(
            sniff_media_type_for_path(PDF, Path::new("/tmp/x.bin")),
            "application/pdf"
        );
    }
}
