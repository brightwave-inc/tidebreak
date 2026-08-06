import { useMemo } from "react";
import ReactMarkdown from "react-markdown";
import rehypeHighlight from "rehype-highlight";

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
    case "application/vnd.openwave.chart+json":
      return "json";
    case "text/html":
      return "xml";
    default:
      return "plaintext";
  }
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
}: {
  content: string;
  mediaType: string;
}) {
  const language = codeLanguageForMediaType(mediaType);
  const markdown = useMemo(
    () => fence(language, content),
    [language, content],
  );

  return (
    <div className="message-markdown code-viewer">
      <ReactMarkdown rehypePlugins={[rehypeHighlight]}>{markdown}</ReactMarkdown>
    </div>
  );
}
