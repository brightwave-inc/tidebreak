/**
 * The media type a file name implies, for a surface that has the name but not
 * the type yet.
 *
 * An import in flight is exactly that: the host reports a display name as each
 * file streams, and the sniffed media type only exists once the file has landed
 * and been catalogued. Guessing from the extension is what lets the queue show
 * a Word file's own mark rather than a generic page for every row.
 *
 * A guess, and only used to pick a glyph. Nothing dispatches a viewer or a
 * parser on this — those wait for the type the bytes were sniffed as.
 */
const BY_EXTENSION: Record<string, string> = {
  pdf: "application/pdf",
  doc: "application/msword",
  docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  xls: "application/vnd.ms-excel",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  csv: "text/csv",
  tsv: "text/tab-separated-values",
  ppt: "application/vnd.ms-powerpoint",
  pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  md: "text/markdown",
  markdown: "text/markdown",
  txt: "text/plain",
  json: "application/json",
  xml: "application/xml",
  html: "text/html",
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  heic: "image/heic",
  mp3: "audio/mpeg",
  wav: "audio/wav",
  m4a: "audio/mp4",
};

export function mediaTypeForFileName(fileName: string): string {
  const extension = fileName.split(".").pop();
  if (!extension || extension === fileName) return "application/octet-stream";
  return BY_EXTENSION[extension.toLowerCase()] ?? "application/octet-stream";
}
