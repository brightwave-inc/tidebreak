import type { CitationLocator } from "./api";
import type { Element, ElementContent, Root, RootContent } from "hast";

const OPENING = ":cit[";
const CLOSING = /^\]\{([^}]*)\}/;
const MAX_PHRASE_CHARACTERS = 512;
const UUID =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const CELL_RANGE = /^[A-Z]+[1-9][0-9]*(?::[A-Z]+[1-9][0-9]*)?$/i;

/** Properties carrying a validated locator from the hast tree to React. */
export const CITATION_DOCUMENT_PROPERTY = "dataCitationDocument";
export const CITATION_LOCATOR_PROPERTY = "dataCitationLocator";

/** Reduce stored citations, including the historical citation-id form, to prose. */
export function stripCitationDirectives(input: string): string {
  return input.replace(/:cit\[([^\]]*)\]\{[^}]*\}/g, "$1");
}

/** Whether text could carry a citation at all — a cheap conservative test. */
export function hasCitationDirective(input: string): boolean {
  return input.includes(OPENING);
}

/**
 * Turn model-authored locator directives into spans while preserving any
 * Markdown inside the cited phrase. Historical citation-id directives become
 * bare prose: their evidence rows no longer exist, but their text still reads.
 */
export function rehypeCitationDirectives() {
  return (tree: Root) => {
    tree.children = convertContent(elementContent(tree.children));
  };
}

function convertContent(nodes: ElementContent[]): ElementContent[] {
  const converted: ElementContent[] = [];
  let index = 0;
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
      pushText(converted, OPENING);
      carry = phrase;
      continue;
    }

    index += citation.consumed;
    if (citation.attributes.kind === "legacy") {
      converted.push(...citation.children);
    } else {
      converted.push({
        type: "element",
        tagName: "span",
        properties: {
          [CITATION_DOCUMENT_PROPERTY]: citation.attributes.documentId,
          [CITATION_LOCATOR_PROPERTY]: JSON.stringify(citation.attributes.locator),
        },
        children: citation.children,
      });
    }
    carry = citation.trailing;
  }

  return converted;
}

type CitationAttributes =
  | { kind: "legacy" }
  | { kind: "locator"; documentId: string; locator: CitationLocator };

type ScannedCitation = {
  children: ElementContent[];
  attributes: CitationAttributes;
  consumed: number;
  trailing: string;
};

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
      const attributes = parseAttributes(closing[1] ?? "");
      if (!attributes) return null;
      pushText(children, text.slice(0, close));
      return {
        children,
        attributes,
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
    if (next.type !== "element") return null;
    children.push(next);
    text = "";
  }
}

function parseAttributes(raw: string): CitationAttributes | null {
  const values = new Map<string, string>();
  const token = /([a-z_]+)=(?:"([^"]*)"|([^\s]+))/gy;
  let offset = 0;
  while (offset < raw.length) {
    while (raw[offset] === " ") offset += 1;
    if (offset >= raw.length) break;
    token.lastIndex = offset;
    const match = token.exec(raw);
    if (!match || values.has(match[1]!)) return null;
    values.set(match[1]!, match[2] ?? match[3] ?? "");
    offset = token.lastIndex;
  }

  if (
    values.size === 1 &&
    values.has("citation_id") &&
    UUID.test(values.get("citation_id") ?? "")
  ) {
    return { kind: "legacy" };
  }

  const documentId = values.get("doc");
  if (!documentId || !UUID.test(documentId)) return null;

  const locatorKeys = ["page", "pages", "lines", "sheet"].filter((key) =>
    values.has(key),
  );
  if (locatorKeys.length > 1) return null;
  if (values.has("cells") && !values.has("sheet")) return null;
  const allowed = new Set(["doc", ...locatorKeys, ...(values.has("cells") ? ["cells"] : [])]);
  if ([...values.keys()].some((key) => !allowed.has(key))) return null;

  let locator: CitationLocator = { kind: "document" };
  if (values.has("page")) {
    const page = positiveInteger(values.get("page"));
    if (page === null) return null;
    locator = { kind: "page", page };
  } else if (values.has("pages")) {
    const range = numberRange(values.get("pages"));
    if (!range) return null;
    locator = { kind: "pages", start: range[0], end: range[1] };
  } else if (values.has("lines")) {
    const range = numberRange(values.get("lines"));
    if (!range) return null;
    locator = { kind: "lines", start: range[0], end: range[1] };
  } else if (values.has("sheet")) {
    const sheet = values.get("sheet")?.trim();
    const cells = values.get("cells") ?? null;
    if (!sheet || (cells !== null && !CELL_RANGE.test(cells))) return null;
    locator = { kind: "sheet", sheet, cells };
  }

  return { kind: "locator", documentId, locator };
}

function positiveInteger(value: string | undefined): number | null {
  if (!value || !/^[1-9][0-9]*$/.test(value)) return null;
  const number = Number(value);
  return Number.isSafeInteger(number) ? number : null;
}

function numberRange(value: string | undefined): [number, number] | null {
  const match = /^([1-9][0-9]*)-([1-9][0-9]*)$/.exec(value ?? "");
  const start = positiveInteger(match?.[1]);
  const end = positiveInteger(match?.[2]);
  return start !== null && end !== null && start <= end ? [start, end] : null;
}

const OPAQUE_TAGS = new Set(["code", "pre"]);

function isOpaque(node: Element): boolean {
  if (OPAQUE_TAGS.has(node.tagName)) return true;
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
