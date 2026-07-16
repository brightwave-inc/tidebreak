import { memo } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

/**
 * Keep model-generated navigation deliberately narrow. `react-markdown` does
 * not render raw HTML unless a raw HTML plugin is opted into (we do not), and
 * this allowlist keeps rendered links from opening local files or executable
 * schemes.
 */
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
  code: ({ children }) => <code>{children}</code>,
  pre: ({ children }) => <pre>{children}</pre>,
  blockquote: ({ children }) => <blockquote>{children}</blockquote>,
  table: ({ children }) => (
    <div className="markdown-table-wrap">
      <table>{children}</table>
    </div>
  ),
};

interface MessageMarkdownProps {
  children: string;
}

export const MessageMarkdown = memo(function MessageMarkdown({
  children,
}: MessageMarkdownProps) {
  return (
    <div className="message-markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={components}
        skipHtml
        urlTransform={(url) => safeMarkdownUrl(url) ?? ""}
      >
        {children}
      </ReactMarkdown>
    </div>
  );
});
