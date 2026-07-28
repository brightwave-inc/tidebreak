import { memo, useMemo } from "react";
import ReactMarkdown, {
  type Components,
  type Options,
} from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeHighlight from "rehype-highlight";
import rehypeKatex from "rehype-katex";
import { ClipboardCopyButton } from "./ClipboardCopyButton";
import { MarkdownTable } from "./MarkdownTable";
import { splitMarkdownBlocks } from "./markdownBlocks";
import { escapeLatexText } from "./markdownLatex";

/**
 * Keep model-generated navigation deliberately narrow. `react-markdown` does
 * not render raw HTML unless a raw HTML plugin is opted into (we do not), and
 * this allowlist keeps rendered links from opening local files or executable
 * schemes.
 */
/**
 * Convert single newlines to Markdown hard breaks (two trailing spaces + newline)
 * so a model's intended line breaks render, while double+ newlines stay paragraph
 * breaks. This is what lets us drop `white-space: pre-wrap` on the container: the
 * line breaks flow through the parser instead of being forced by CSS, so source
 * indentation no longer leaks and the parsed block structure renders cleanly.
 */
export function preserveLineBreaks(input: string): string {
  // Fenced code is source text: hard-break spaces would corrupt what the
  // block renders and what the copy control yields. The `$` alternative keeps
  // a still-streaming, unclosed fence untouched too.
  return input
    .split(/(```[\s\S]*?(?:```|$))/)
    .map((segment) =>
      segment.startsWith("```")
        ? segment
        : segment.replace(/([^\n])\n(?!\n)/g, "$1  \n"),
    )
    .join("");
}

/**
 * The raw source of a code block: concatenated text descendants of the hast
 * node, ignoring the token spans highlighting wraps them in. What the copy
 * button writes — never the highlighted markup.
 */
export function rawCodeText(node: {
  children?: unknown[];
  value?: unknown;
  type?: unknown;
}): string {
  if (node.type === "text" && typeof node.value === "string") {
    return node.value;
  }
  if (!Array.isArray(node.children)) return "";
  return node.children
    .map((child) => rawCodeText(child as { children?: unknown[] }))
    .join("");
}

export function safeMarkdownUrl(url: string | undefined): string | undefined {
  if (!url) return undefined;

  try {
    const parsed = new URL(url);
    return parsed.protocol === "https:" ? parsed.href : undefined;
  } catch {
    return undefined;
  }
}

const components: Components = {
  p: ({ children }) => <p>{children}</p>,
  h1: ({ children }) => <h1>{children}</h1>,
  h2: ({ children }) => <h2>{children}</h2>,
  h3: ({ children }) => <h3>{children}</h3>,
  h4: ({ children }) => <h4>{children}</h4>,
  h5: ({ children }) => <h5>{children}</h5>,
  h6: ({ children }) => <h6>{children}</h6>,
  a: ({ children, href }) => {
    const safeHref = safeMarkdownUrl(href);
    if (!safeHref) return <span>{children}</span>;

    return (
      <a href={safeHref} target="_blank" rel="noreferrer noopener">
        {children}
      </a>
    );
  },
  // Do not let assistant Markdown initiate unrequested network loads. The alt
  // text remains available as a small, readable indication of omitted media.
  img: ({ alt }) => (
    <span className="markdown-image-omitted" role="note">
      {alt ? `Image omitted: ${alt}` : "Image omitted"}
    </span>
  ),
  // className carries the fence language plus highlight token classes; the
  // spans rehype-highlight nests inside render through the defaults.
  code: ({ children, className }) => (
    <code className={className}>{children}</code>
  ),
  pre: ({ children, node }) => {
    const source = node ? rawCodeText(node) : "";
    return (
      <div className="code-block">
        {source && (
          <ClipboardCopyButton
            value={source}
            label="Copy code"
            copiedAnnouncement="Code copied"
            failedAnnouncement="Copy failed"
            className="code-block-copy"
          />
        )}
        <pre>{children}</pre>
      </div>
    );
  },
  blockquote: ({ children }) => <blockquote>{children}</blockquote>,
  table: ({ children }) => <MarkdownTable>{children}</MarkdownTable>,
};

// `singleDollarTextMath: false` keeps a bare `$` (a price, a shell variable)
// from being read as a math delimiter; display and inline math still arrive
// through the `$$…$$` / `$…$` forms that `escapeLatexText` normalizes to.
const remarkPlugins: Options["remarkPlugins"] = [
  remarkGfm,
  [remarkMath, { singleDollarTextMath: false }],
];

const rehypePlugins: Options["rehypePlugins"] = [
  // Highlight only fence-tagged languages; auto-detection on unlabeled blocks
  // guesses wrong too often to be worth it.
  [rehypeHighlight, { detect: false }],
  rehypeKatex,
];

/**
 * Normalize raw model output before the parser sees it: rewrite LaTeX
 * delimiters into the dollar forms remark-math understands, then convert single
 * newlines into hard breaks so intended line breaks survive without leaking
 * source indentation.
 */
export function processMarkdownContent(input: string): string {
  return preserveLineBreaks(escapeLatexText(input));
}

/**
 * One top-level Markdown block, memoized on its verbatim source so that a
 * streaming message — whose text grows a few characters every typewriter tick —
 * only re-runs the remark/rehype pipeline for its trailing (still-growing)
 * block. Every settled block above it hits this memo and is left untouched,
 * turning a per-tick full-document re-parse into work proportional to the tail.
 */
const MarkdownBlock = memo(function MarkdownBlock({ block }: { block: string }) {
  const processed = useMemo(() => processMarkdownContent(block), [block]);
  return (
    <ReactMarkdown
      remarkPlugins={remarkPlugins}
      rehypePlugins={rehypePlugins}
      components={components}
      skipHtml
      urlTransform={(url) => safeMarkdownUrl(url) ?? ""}
    >
      {processed}
    </ReactMarkdown>
  );
});

interface MessageMarkdownProps {
  children: string;
}

export const MessageMarkdown = memo(function MessageMarkdown({
  children,
}: MessageMarkdownProps) {
  const blocks = useMemo(() => splitMarkdownBlocks(children), [children]);
  return (
    <div className="message-markdown">
      {blocks.map((block, index) => (
        // Blocks are append-only while streaming: the prefix is immutable and
        // only the tail grows, so the array index is a stable identity that
        // keeps the growing block mounted across ticks.
        <MarkdownBlock key={index} block={block} />
      ))}
    </div>
  );
});
