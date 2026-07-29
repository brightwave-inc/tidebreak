import type { CitationFormat } from "./api";

/**
 * The formats a picker offers, in the order it lists them.
 *
 * Both end with the same sources row; what differs is whether the answer also
 * carries an anchor on every claim. The labels say that rather than naming the
 * mechanism, because the mechanism is not what a reader is choosing between.
 */
export const CITATION_FORMAT_OPTIONS: {
  value: CitationFormat;
  label: string;
  description: string;
}[] = [
  {
    value: "inline",
    label: "Inline",
    description:
      "Anchor each claim to the source behind it, and list the sources at the end.",
  },
  {
    value: "sources_attached",
    label: "Sources only",
    description:
      "Answer plainly, with no anchors in the prose, and list the sources at the end.",
  },
];

export const CITATION_FORMAT_LABELS: Record<CitationFormat, string> =
  Object.fromEntries(
    CITATION_FORMAT_OPTIONS.map((option) => [option.value, option.label]),
  ) as Record<CitationFormat, string>;
