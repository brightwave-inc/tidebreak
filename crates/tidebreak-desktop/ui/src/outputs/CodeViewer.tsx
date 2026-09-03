import { useMemo } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";

import { highlightRehypeOptions } from "@/highlightLanguages";

// Map source extensions onto the highlighter's registered grammars. Extensions
// outside that subset still open in the source viewer, with the media-type
// fallback selecting plain text.
const LANGUAGE_BY_EXTENSION: Readonly<Record<string, string>> = {
  py: "python",
  pyw: "python",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  cts: "typescript",
  rs: "rust",
  go: "go",
  java: "java",
  c: "c",
  h: "c",
  cc: "cpp",
  cpp: "cpp",
  cxx: "cpp",
  hpp: "cpp",
  hxx: "cpp",
  cs: "csharp",
  rb: "ruby",
  php: "php",
  swift: "swift",
  kt: "kotlin",
  kts: "kotlin",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  fish: "bash",
  sql: "sql",
  css: "css",
  scss: "scss",
  sass: "scss",
  less: "less",
  vue: "xml",
  svelte: "xml",
  toml: "ini",
  yaml: "yaml",
  yml: "yaml",
  xml: "xml",
  graphql: "graphql",
  gql: "graphql",
  lua: "lua",
  r: "r",
  pl: "perl",
  pm: "perl",
};

/**
 * Fence long enough that no run of backticks inside `content` can close it.
 * Markdown fences match the longest opening run, so stretching past the
 * content's longest run is enough — no escaping of the body.
 */
function fence(language: string, content: string): string {
  let ticks = "```";
  while (content.includes(ticks)) ticks += "`";
  return `${ticks}${language}\n${content}\n${ticks}`;
}

/** Highlight language for a curated text output's media type. */
export function codeLanguageForMediaType(mediaType: string): string {
  switch (mediaType.split(";", 1)[0]!.trim().toLowerCase()) {
    case "application/json":
    case "application/vnd.tidebreak.chart+json":
      return "json";
    case "text/html":
      return "xml";
    default:
      return "plaintext";
  }
}

/** Highlight language for a source filename, when the extension identifies one. */
export function codeLanguageForFilename(filename: string): string | null {
  const lower = filename.toLowerCase();
  if (["dockerfile", "makefile", "justfile"].includes(lower)) {
    return lower === "dockerfile" ? "bash" : "makefile";
  }
  const extension = lower.includes(".") ? lower.split(".").pop()! : "";
  return LANGUAGE_BY_EXTENSION[extension] ?? null;
}

/**
 * Syntax-highlighted source view for curated text outputs that are not
 * markdown (JSON, HTML-as-source, plain text).
 *
 * Reuses the chat fence highlighter and the `.message-markdown` token colors
 * so an output and a fenced block in the transcript read the same.
 */
export function CodeViewer({
  content,
  mediaType,
  filename,
}: {
  content: string;
  mediaType: string;
  filename?: string;
}) {
  const language =
    (filename && codeLanguageForFilename(filename)) ??
    codeLanguageForMediaType(mediaType);
  const markdown = useMemo(() => fence(language, content), [language, content]);

  return (
    <div className="message-markdown code-viewer">
      <ReactMarkdown
        rehypePlugins={[[rehypeHighlight, highlightRehypeOptions]]}
      >
        {markdown}
      </ReactMarkdown>
    </div>
  );
}
