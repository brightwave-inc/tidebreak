import {
  isValidElement,
  memo,
  useMemo,
  useRef,
  type ReactElement,
  type ReactNode,
} from "react";
import ReactMarkdown, {
  type Components,
  type Options,
} from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkMath from "remark-math";
import rehypeHighlight from "rehype-highlight";
import rehypeKatex from "rehype-katex";
import { ClipboardCopyButton, copyRichText } from "./ClipboardCopyButton";
import {
  Table,
  TableBody,
  TableCaption,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  CITATION_DOCUMENT_PROPERTY,
  CITATION_LOCATOR_PROPERTY,
  hasCitationDirective,
  rehypeCitationDirectives,
} from "./citationDirectives";
import type { CitationLocator } from "./api";
import {
  pieceStartOffsets,
  rangeWithinPiece,
  rehypeHighlightRange,
  type HighlightRange,
} from "./components/document/citationMark";
import { InlineCitation } from "./InlineCitation";
import { splitMarkdownBlocks } from "./markdownBlocks";
import { escapeLatexText } from "./markdownLatex";
import { slugify } from "./markdownHeadings";

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
  const points = hardBreakInsertions(input);
  if (points.length === 0) return input;

  let result = "";
  let from = 0;
  for (const point of points) {
    result += input.slice(from, point) + "  ";
    from = point;
  }
  return result + input.slice(from);
}

/**
 * The offsets in `input` that {@link preserveLineBreaks} inserts two spaces
 * before — every single newline outside a fence.
 *
 * The rewrite is defined in terms of these positions rather than the other way
 * round, so a caller that needs to know where a source offset ended up after it
 * — placing a citation's mark in the parsed tree — asks the same scan instead of
 * modelling the transform a second time and drifting from it.
 */
function hardBreakInsertions(input: string): number[] {
  const points: number[] = [];
  let offset = 0;
  // Fenced code is source text: hard-break spaces would corrupt what the block
  // renders and what the copy control yields. The `$` alternative keeps a
  // still-streaming, unclosed fence untouched too.
  for (const segment of input.split(/(```[\s\S]*?(?:```|$))/)) {
    if (!segment.startsWith("```")) {
      const singleNewline = /[^\n]\n(?!\n)/g;
      let match: RegExpExecArray | null;
      while ((match = singleNewline.exec(segment)) !== null) {
        points.push(offset + match.index + 1);
      }
    }
    offset += segment.length;
  }
  return points;
}

/**
 * Where an offset in the Markdown source lands in the string the parser is
 * handed — the same offset plus the hard breaks inserted ahead of it.
 *
 * Only the line-break rewrite shifts anything: {@link escapeLatexText} rewrites
 * LaTeX delimiters, so a caller that means to address the parsed tree has to
 * establish that it left the source alone before this arithmetic means
 * anything.
 */
export function hardBreakOffset(input: string, offset: number): number {
  let shift = 0;
  for (const point of hardBreakInsertions(input)) {
    if (point >= offset) break;
    shift += 2;
  }
  return offset + shift;
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

/**
 * The tab-separated form of a rendered table — what a spreadsheet, an editor,
 * or a terminal receives when the table is copied.
 *
 * Read off the DOM rather than the Markdown source: what the reader sees is
 * already the resolved cell text, inline formatting and citations included,
 * and re-parsing the source would be a second renderer to keep in agreement
 * with this one.
 */
export function tableClipboardText(table: HTMLTableElement): string {
  return Array.from(table.rows)
    .map((row) =>
      Array.from(row.cells)
        .map((cell) => (cell.textContent ?? "").replace(/\s+/g, " ").trim())
        .join("\t"),
    )
    .join("\n");
}

async function copyTable(table: HTMLTableElement | null): Promise<void> {
  if (!table) throw new Error("No table to copy");
  await copyRichText(table.outerHTML, tableClipboardText(table));
}

/**
 * A Markdown table, in its own horizontal scroll container so a wide table
 * scrolls instead of crushing its columns below the message width, with a copy
 * control that yields the table in both a rich and a plain flavour.
 *
 * The control reads the table out of a ref at click time, so this stays a pure
 * render with no effect that would assume a complete table — the block is
 * re-rendered on every streaming tick, half-parsed rows and all.
 */
function MarkdownTable({ children }: { children?: ReactNode }) {
  const container = useRef<HTMLDivElement>(null);

  return (
    <div className="group/markdown-table relative my-5">
      <ClipboardCopyButton
        copy={() => copyTable(container.current?.querySelector("table") ?? null)}
        label="Copy table"
        copiedAnnouncement="Table copied"
        failedAnnouncement="Copy failed"
        className="border-border bg-background text-muted-foreground hover:text-foreground absolute top-0 right-0 z-10 inline-flex items-center rounded-md border p-1 opacity-0 shadow-sm transition-opacity group-hover/markdown-table:opacity-100 focus-visible:opacity-100"
      />
      <div ref={container} className="w-full overflow-x-auto">
        <Table>{children}</Table>
      </div>
    </div>
  );
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
  thead: ({ children }) => <TableHeader>{children}</TableHeader>,
  tbody: ({ children }) => <TableBody>{children}</TableBody>,
  tr: ({ children }) => <TableRow>{children}</TableRow>,
  // GFM column alignment arrives as an inline text-align style; forwarding it
  // is what keeps a right-aligned numeric column right-aligned.
  th: ({ children, style }) => <TableHead style={style}>{children}</TableHead>,
  td: ({ children, style }) => <TableCell style={style}>{children}</TableCell>,
  caption: ({ children }) => <TableCaption>{children}</TableCaption>,
  // Most spans in a rendered message are syntax-highlighting tokens and pass
  // straight through; the ones {@link rehypeCitationDirectives} built carry the
  // citation they cite and become the phrase a reader can open.
  span: ({ children, node, ...props }) => {
    const documentId = node?.properties?.[CITATION_DOCUMENT_PROPERTY];
    const rawLocator = node?.properties?.[CITATION_LOCATOR_PROPERTY];
    const locator =
      typeof rawLocator === "string" ? parseCitationLocator(rawLocator) : null;
    if (typeof documentId !== "string" || !locator) {
      return <span {...props}>{children}</span>;
    }
    return (
      <InlineCitation documentId={documentId} locator={locator}>
        {children}
      </InlineCitation>
    );
  },
};

function parseCitationLocator(value: string): CitationLocator | null {
  try {
    const parsed: unknown = JSON.parse(value);
    if (!parsed || typeof parsed !== "object" || !("kind" in parsed)) return null;
    const kind = (parsed as { kind?: unknown }).kind;
    return ["document", "page", "pages", "lines", "sheet"].includes(
      String(kind),
    )
      ? (parsed as CitationLocator)
      : null;
  } catch {
    return null;
  }
}

/**
 * The same components, with every heading carrying the slug id derived from its
 * own text — which is how the source viewer's outline finds a section to scroll
 * to without the two sides agreeing on anything but {@link slugify}.
 *
 * Opt-in rather than always on: a transcript renders many messages into one
 * document, and headings from different messages would collide on id.
 */
const componentsWithHeadingIds: Components = {
  ...components,
  ...Object.fromEntries(
    (["h1", "h2", "h3", "h4", "h5", "h6"] as const).map((tag) => [
      tag,
      ({ children }: { children?: ReactNode }) => {
        const Tag = tag;
        return <Tag id={slugify(headingText(children))}>{children}</Tag>;
      },
    ]),
  ),
};

/** A heading's plain text, from whatever inline nodes it was rendered as. */
function headingText(children: ReactNode): string {
  if (typeof children === "string") return children;
  if (typeof children === "number") return String(children);
  if (Array.isArray(children)) return children.map(headingText).join("");
  if (isValidElement<{ children?: ReactNode }>(children)) {
    return headingText(children.props.children);
  }
  return "";
}

// `singleDollarTextMath: false` keeps a bare `$` (a price, a shell variable)
// from being read as a math delimiter; display and inline math still arrive
// through the `$$…$$` / `$…$` forms that `escapeLatexText` normalizes to.
const remarkPlugins: Options["remarkPlugins"] = [
  remarkGfm,
  [remarkMath, { singleDollarTextMath: false }],
];

const rehypePlugins: NonNullable<Options["rehypePlugins"]> = [
  // Before anything rewrites the tree: the citations are read out of text nodes
  // as the parser left them, which highlighting and math no longer are.
  rehypeCitationDirectives,
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
 *
 * Citations are deliberately not touched here. They are read off the parsed
 * tree, where the phrase they wrap is already the inline nodes it was written
 * as, rather than off the source, where taking them apart would mean parsing
 * Markdown twice.
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
const MarkdownBlock = memo(function MarkdownBlock({
  block,
  headingIds,
  highlightStart,
  highlightEnd,
  wrapBlock,
}: {
  block: string;
  headingIds: boolean;
  /** Range to mark, in this block's own source. Primitives, so memo compares. */
  highlightStart?: number;
  highlightEnd?: number;
  /** Stable across renders, or every block re-parses on every tick. */
  wrapBlock?: WrapMarkdownBlock;
}) {
  const processed = useMemo(() => processMarkdownContent(block), [block]);
  const plugins = useMemo(
    () => blockRehypePlugins(block, highlightStart, highlightEnd),
    [block, highlightStart, highlightEnd],
  );
  const blockComponents = useMemo(() => {
    const base = headingIds ? componentsWithHeadingIds : components;
    const wrapping = wrappingComponents(wrapBlock, processed);
    return wrapping ? { ...base, ...wrapping } : base;
  }, [headingIds, wrapBlock, processed]);
  return (
    <ReactMarkdown
      remarkPlugins={remarkPlugins}
      rehypePlugins={plugins}
      components={blockComponents}
      skipHtml
      urlTransform={(url) => safeMarkdownUrl(url) ?? ""}
    >
      {processed}
    </ReactMarkdown>
  );
});

/**
 * The pipeline for one block, with the citation's mark added when the block
 * holds one.
 *
 * The marker runs before the rest because it is the only pass that depends on
 * text nodes still standing where they were read from: syntax highlighting and
 * math rewrite their subtrees and drop those positions, so a passage inside a
 * code fence or a formula goes unmarked rather than marked by guesswork.
 *
 * A block whose LaTeX delimiters the pipeline rewrites is left alone for the
 * same reason — the rewrite moves text the offsets were measured against, and
 * only the hard-break insertion can be accounted for.
 */
function blockRehypePlugins(
  block: string,
  start: number | undefined,
  end: number | undefined,
): NonNullable<Options["rehypePlugins"]> {
  if (start == null || end == null || end <= start) return rehypePlugins;
  if (escapeLatexText(block) !== block) return rehypePlugins;
  // A block carrying a citation is left alone too: the mark would split the
  // text the citation is read out of, leaving the two passes to argue over the
  // same nodes for a passage that is already marked by the citation itself.
  if (hasCitationDirective(block)) return rehypePlugins;
  return [
    [
      rehypeHighlightRange,
      {
        range: {
          start: hardBreakOffset(block, start),
          end: hardBreakOffset(block, end),
        },
      },
    ],
    ...rehypePlugins,
  ];
}

/**
 * Wrap each addressable block of a rendered message in caller-supplied chrome.
 *
 * The caller gets the block's own Markdown source alongside the element, which
 * is what lets a comment be filed against the text a reader pointed at rather
 * than against a rendered node nobody can name. Blocks are the units a person
 * would quote: paragraphs, leaf list items, quotes, and code fences. A list
 * item holding a nested list is left alone — wrapping it would put the chrome
 * around its whole subtree, and its leaves are wrapped individually anyway.
 */
export type WrapMarkdownBlock = (
  source: string,
  element: ReactElement,
) => ReactNode;

/**
 * The block-wrapping overrides for one parsed block, or nothing when the
 * caller wants none.
 *
 * `processed` is what the parser was handed, so the offsets it recorded index
 * into it and not into the caller's original text.
 */
function wrappingComponents(
  wrapBlock: WrapMarkdownBlock | undefined,
  processed: string,
): Components | undefined {
  if (!wrapBlock) return undefined;
  const wrap = (
    node: { position?: { start: { offset?: number }; end: { offset?: number } } } | undefined,
    element: ReactElement,
  ): ReactNode => {
    const start = node?.position?.start.offset;
    const end = node?.position?.end.offset;
    const source =
      start != null && end != null ? processed.slice(start, end) : "";
    return wrapBlock(source, element);
  };
  return {
    p: ({ node, children }) => wrap(node, <p>{children}</p>),
    li: ({ node, children }) => {
      const nested = node?.children?.some(
        (child) =>
          child.type === "element" &&
          (child.tagName === "ul" || child.tagName === "ol"),
      );
      return nested ? <li>{children}</li> : wrap(node, <li>{children}</li>);
    },
    blockquote: ({ node, children }) =>
      wrap(node, <blockquote>{children}</blockquote>),
    // The fence keeps its copy button; the chrome goes around the whole block
    // so it does not land on top of it.
    pre: ({ node, children }) => {
      const source = node ? rawCodeText(node) : "";
      return wrap(
        node,
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
        </div>,
      );
    },
  };
}

interface MessageMarkdownProps {
  children: string;
  /**
   * Give every heading a slug id, for a caller that means to scroll to one.
   * Off for transcripts, where headings from separate messages would collide.
   */
  headingIds?: boolean;
  /**
   * Half-open character range of `children` to mark as the cited passage.
   *
   * Addresses the source rather than the rendering, which is what makes it
   * answerable: the range is carried down to whichever blocks it covers and
   * placed against the offsets the parser recorded for each of them.
   */
  highlightRange?: HighlightRange;
  /**
   * Wrap every addressable block in caller-supplied chrome — the plan card's
   * per-block comment affordance. Must be stable (`useCallback`), or each
   * render re-parses every block.
   */
  wrapBlock?: WrapMarkdownBlock;
}

export const MessageMarkdown = memo(function MessageMarkdown({
  children,
  headingIds = false,
  highlightRange,
  wrapBlock,
}: MessageMarkdownProps) {
  const blocks = useMemo(() => splitMarkdownBlocks(children), [children]);
  const blockStarts = useMemo(() => pieceStartOffsets(blocks), [blocks]);

  return (
    <div className="message-markdown">
      {blocks.map((block, index) => {
        const inBlock = highlightRange
          ? rangeWithinPiece(highlightRange, blockStarts[index]!, block.length)
          : null;
        return (
          // Blocks are append-only while streaming: the prefix is immutable and
          // only the tail grows, so the array index is a stable identity that
          // keeps the growing block mounted across ticks.
          <MarkdownBlock
            key={index}
            block={block}
            headingIds={headingIds}
            highlightStart={inBlock?.start}
            highlightEnd={inBlock?.end}
            wrapBlock={wrapBlock}
          />
        );
      })}
    </div>
  );
});
