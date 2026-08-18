import { InlineCitation, MessageCitationsProvider } from "tidebreak-desktop-ui";

const sources = [
  {
    id: "cit-1",
    ordinal: 1,
    documentId: "doc-quarterly-report",
    locator: { kind: "page", page: 4 },
  },
  {
    id: "cit-2",
    ordinal: 2,
    documentId: "doc-retry-rs",
    locator: { kind: "lines", start: 118, end: 131 },
  },
] as const;

const citations = {
  sources,
  onOpenSource: () => {},
};

// At rest the phrase carries no band and the chip stays neutral; the accent
// band grows and the chip fills only while the citation is hovered or open. A
// static capture never hovers, so one cell pins the open appearance.
const openState = `
.ds-citation-open .inline-citation-phrase { background-size: 100% 35%; }
.ds-citation-open .inline-citation-mark { background: var(--brand-accent); }
.ds-citation-open .inline-citation-glyph { fill: var(--citation-mark-active-ink); }
`;

export function CitedSentence() {
  return (
    <MessageCitationsProvider value={citations}>
      <article
        className="message message-assistant"
        style={{ maxWidth: "40rem" }}
      >
        <div className="message-markdown">
          <p>
            The report puts{" "}
            <InlineCitation
              documentId="doc-quarterly-report"
              locator={{ kind: "page", page: 4 }}
            >
              Q2 infrastructure spend at $412k, up 9% quarter over quarter
            </InlineCitation>
            , which matches the invoice totals in the finance folder.
          </p>
        </div>
      </article>
    </MessageCitationsProvider>
  );
}

export function LineRangeCitation() {
  return (
    <MessageCitationsProvider value={citations}>
      <article
        className="message message-assistant ds-citation-open"
        style={{ maxWidth: "40rem" }}
      >
        <style>{openState}</style>
        <div className="message-markdown">
          <p>
            The retry loop{" "}
            <InlineCitation
              documentId="doc-retry-rs"
              locator={{ kind: "lines", start: 118, end: 131 }}
            >
              registers its timer only after yielding to the executor
            </InlineCitation>
            , which is exactly the ordering the flaky test trips over.
          </p>
        </div>
      </article>
    </MessageCitationsProvider>
  );
}

export function WithoutSource() {
  return (
    <MessageCitationsProvider value={citations}>
      <article
        className="message message-assistant"
        style={{ maxWidth: "40rem" }}
      >
        <div className="message-markdown">
          <p>
            A phrase whose document is no longer attached{" "}
            <InlineCitation
              documentId="doc-missing"
              locator={{ kind: "document" }}
            >
              renders as plain prose with no chip
            </InlineCitation>
            , so a stale citation never breaks the sentence.
          </p>
        </div>
      </article>
    </MessageCitationsProvider>
  );
}
