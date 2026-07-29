import type { Element, ElementContent, Root, RootContent } from "hast";

/**
 * How a stored citation wraps the phrase it backs: `:cit[phrase]{citation_id=…}`.
 *
 * The grammar is closed on the writing side — the parser that produces this form
 * closes the phrase at its first `]`, so no `]` can occur inside one — which is
 * what makes reading it back a scan rather than a parse, and is why this is a
 * few dozen lines here instead of a Markdown directive extension over every
 * message the model writes.
 */
const OPENING = ":cit[";

/**
 * The closing, anchored at the `]` that ended the phrase. The id is taken as
 * written and validated by the renderer: an id that is not a citation this
 * message carries still had its phrase authored as prose, and prose is what it
 * reads as.
 */
const CLOSING = /^\]\{citation_id=([^}\s]*)\}/;

/**
 * How far a phrase is followed before the opening is read as ordinary prose.
 * A citation wraps a clause the model just wrote; anything longer is text that
 * happens to begin like one. Mirrors the streaming scrubber's own bound.
 */
const MAX_PHRASE_CHARACTERS = 512;

/** Where the citation's id is carried from the tree to the component. */
export const CITATION_ID_PROPERTY = "dataCitationId";

/**
 * The stored form, matched whole, for a caller that means to remove citations
 * rather than render them — the clipboard, which yields prose and not markup.
 */
const STORED_CITATION =
  /:cit\[([^\]]*)\]\{citation_id=[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\}/gi;

/** Reduce stored citations to the phrasing they wrap. */
export function stripCitationDirectives(input: string): string {
  return input.replace(STORED_CITATION, "$1");
}

/** Whether text could carry a citation at all — a cheap conservative test. */
export function hasCitationDirective(input: string): boolean {
  return input.includes(OPENING);
}

/**
 * Turn stored citations into spans carrying the id they cite, leaving the cited
 * phrase where the model wrote it.
 *
 * This runs over the parsed tree rather than the source so that a phrase with
 * Markdown in it — `:cit[the *largest* reef]{citation_id=…}` — keeps its
 * emphasis: the phrase is whatever inline nodes stand between the opening and
 * the closing, which is why the scan follows siblings rather than matching one
 * text node. Anything that does not close is left exactly as it was read, so
 * directive-shaped prose survives as prose.
 *
 * Code and math are skipped. Their text is source that is either displayed
 * verbatim or handed to another renderer, and rewriting nodes inside them would
 * corrupt what those render.
 */
export function rehypeCitationDirectives() {
  return (tree: Root) => {
    tree.children = convertContent(elementContent(tree.children));
  };
}

function convertContent(nodes: ElementContent[]): ElementContent[] {
  const converted: ElementContent[] = [];
  let index = 0;
  // Text left over from a citation just closed, or from an opening that turned
  // out to be prose, which has to be rescanned rather than emitted.
  let carry: string | null = null;

  while (index < nodes.length || carry !== null) {
    let value: string;
    if (carry !== null) {
      value = carry;
      carry = null;
    } else {
      const node = nodes[index]!;
      index += 1;
      if (node.type === "element") {
        if (!isOpaque(node)) node.children = convertContent(node.children);
        converted.push(node);
        continue;
      }
      if (node.type !== "text") {
        converted.push(node);
        continue;
      }
      value = node.value;
    }

    const opening = value.indexOf(OPENING);
    if (opening === -1) {
      pushText(converted, value);
      continue;
    }

    pushText(converted, value.slice(0, opening));
    const phrase = value.slice(opening + OPENING.length);
    const citation = scanCitation(phrase, nodes.slice(index));
    if (!citation) {
      // Not a citation after all. Release the opening as the prose it is and
      // rescan the rest, so a real citation later in the same text is still
      // found — and so the nodes a failed scan looked at stay untouched.
      pushText(converted, OPENING);
      carry = phrase;
      continue;
    }

    index += citation.consumed;
    converted.push({
      type: "element",
      tagName: "span",
      properties: { [CITATION_ID_PROPERTY]: citation.citationId },
      children: citation.children,
    });
    carry = citation.trailing;
  }

  return converted;
}

type ScannedCitation = {
  /** The phrase, as the inline nodes it was written as. */
  children: ElementContent[];
  citationId: string;
  /** How many following siblings the phrase spanned. */
  consumed: number;
  /** What was left of the text node the citation closed in. */
  trailing: string;
};

/**
 * Read a citation that begins where `head` begins, following siblings until the
 * phrase closes, or `null` for an opening that never closes into one.
 */
function scanCitation(
  head: string,
  following: readonly ElementContent[],
): ScannedCitation | null {
  const children: ElementContent[] = [];
  let text = head;
  let consumed = 0;
  let length = 0;

  for (;;) {
    const close = text.indexOf("]");
    if (close !== -1) {
      const closing = CLOSING.exec(text.slice(close));
      if (!closing) return null;
      pushText(children, text.slice(0, close));
      return {
        children,
        citationId: closing[1] ?? "",
        consumed,
        trailing: text.slice(close + closing[0].length),
      };
    }

    length += text.length;
    if (length > MAX_PHRASE_CHARACTERS) return null;
    pushText(children, text);

    const next = following[consumed];
    if (!next) return null;
    consumed += 1;
    if (next.type === "text") {
      text = next.value;
      continue;
    }
    // A comment cannot be part of a phrase; an element — emphasis, code, a
    // line break — is carried into it whole.
    if (next.type !== "element") return null;
    children.push(next);
    text = "";
  }
}

const OPAQUE_TAGS = new Set(["code", "pre"]);

/** Whether an element's text is source rather than prose. */
function isOpaque(node: Element): boolean {
  if (OPAQUE_TAGS.has(node.tagName)) return true;
  // remark-math parks a formula's source in a span for rehype-katex to render.
  const className = node.properties?.className;
  return Array.isArray(className) && className.includes("math");
}

function elementContent(nodes: RootContent[]): ElementContent[] {
  return nodes.filter(
    (node): node is ElementContent => node.type !== "doctype",
  );
}

function pushText(nodes: ElementContent[], value: string): void {
  if (value) nodes.push({ type: "text", value });
}
