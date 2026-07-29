const MARKER_PREFIX = "[[ow-source:";
const TOKEN_LENGTH = 32;
const MARKER_LENGTH = MARKER_PREFIX.length + TOKEN_LENGTH + 2;

const DIRECTIVE_PREFIX = ":cit[";
const DIRECTIVE_ATTRIBUTE = "]{ref=";
/**
 * How much of an unclosed cited phrase is held before the opening is treated as
 * ordinary prose. A citation wraps a clause the model just wrote, so anything
 * longer is prose that happens to start like a directive — and holding it back
 * would stall the visible stream for the rest of the message.
 */
const MAX_HELD_PHRASE = 512;

/**
 * Removes private source references from streamed assistant text before it is
 * rendered: a bare marker disappears, and a citation directive is reduced to the
 * phrase it wraps, which is what the durable text will read as until the
 * transcript re-renders it as a real citation. Only the tail that could still
 * become a valid reference is retained between deltas; call finish when a stream
 * ends or is interrupted to recover an incomplete one as literal prose.
 */
export class AssistantSourceMarkerStreamScrubber {
  private pending = "";

  push(delta: string): string {
    let input = this.pending + delta;
    let output = "";
    this.pending = "";

    while (input.length > 0) {
      const markerStart = input.indexOf(MARKER_PREFIX);
      const directiveStart = input.indexOf(DIRECTIVE_PREFIX);
      if (markerStart === -1 && directiveStart === -1) {
        const pendingLength = longestPrefixSuffix(input);
        output += input.slice(0, input.length - pendingLength);
        this.pending = input.slice(input.length - pendingLength);
        break;
      }

      const directiveFirst =
        markerStart === -1 ||
        (directiveStart !== -1 && directiveStart < markerStart);
      const start = directiveFirst ? directiveStart : markerStart;
      output += input.slice(0, start);
      const candidate = input.slice(start);

      if (directiveFirst) {
        const directive = scanCitationDirective(candidate);
        if (directive.kind === "complete") {
          output += directive.phrase;
          input = candidate.slice(directive.length);
          continue;
        }
        if (directive.kind === "partial") {
          this.pending = candidate;
          break;
        }
      } else if (candidate.length >= MARKER_LENGTH) {
        const possibleMarker = candidate.slice(0, MARKER_LENGTH);
        if (isSourceMarker(possibleMarker)) {
          input = candidate.slice(MARKER_LENGTH);
          continue;
        }
      } else if (couldBecomeSourceMarker(candidate)) {
        this.pending = candidate;
        break;
      }

      // This looked like a reference opening but cannot become a valid one.
      // Release one character and rescan so malformed prose is preserved and a
      // valid reference beginning later in the same input is still detected.
      output += candidate[0];
      input = candidate.slice(1);
    }

    return output;
  }

  finish(): string {
    const literal = this.pending;
    this.pending = "";
    return literal;
  }
}

type CitationDirectiveScan =
  | { kind: "complete"; phrase: string; length: number }
  | { kind: "partial" }
  | { kind: "invalid" };

/**
 * Classify text that starts with a citation-directive opening: a complete
 * directive with the phrase it wraps, a prefix that could still become one, or
 * prose that cannot.
 *
 * The first `]` closes the phrase, matching the parser that rewrites the durable
 * text, so the two agree on what is a citation and what is prose.
 */
function scanCitationDirective(candidate: string): CitationDirectiveScan {
  const opened = candidate.slice(DIRECTIVE_PREFIX.length);
  const close = opened.indexOf("]");
  if (close === -1) {
    return opened.length > MAX_HELD_PHRASE
      ? { kind: "invalid" }
      : { kind: "partial" };
  }
  if (close > MAX_HELD_PHRASE) return { kind: "invalid" };

  const phrase = opened.slice(0, close);
  const closed = opened.slice(close);
  if (!closed.startsWith(DIRECTIVE_ATTRIBUTE)) {
    return DIRECTIVE_ATTRIBUTE.startsWith(closed)
      ? { kind: "partial" }
      : { kind: "invalid" };
  }

  const attribute = closed.slice(DIRECTIVE_ATTRIBUTE.length);
  const token = attribute.slice(0, TOKEN_LENGTH);
  if (!/^[0-9a-f]*$/.test(token)) return { kind: "invalid" };
  if (attribute.length <= TOKEN_LENGTH) return { kind: "partial" };
  if (attribute[TOKEN_LENGTH] !== "}") return { kind: "invalid" };

  return {
    kind: "complete",
    phrase,
    length:
      DIRECTIVE_PREFIX.length +
      close +
      DIRECTIVE_ATTRIBUTE.length +
      TOKEN_LENGTH +
      1,
  };
}

function isSourceMarker(value: string) {
  if (
    value.length !== MARKER_LENGTH ||
    !value.startsWith(MARKER_PREFIX) ||
    !value.endsWith("]]")
  ) {
    return false;
  }

  const token = value.slice(MARKER_PREFIX.length, -2);
  return /^[0-9a-f]{32}$/.test(token);
}

function couldBecomeSourceMarker(value: string) {
  if (value.length <= MARKER_PREFIX.length) {
    return MARKER_PREFIX.startsWith(value);
  }
  if (!value.startsWith(MARKER_PREFIX) || value.length >= MARKER_LENGTH) {
    return false;
  }

  const tokenEnd = Math.min(
    value.length,
    MARKER_PREFIX.length + TOKEN_LENGTH,
  );
  const token = value.slice(MARKER_PREFIX.length, tokenEnd);
  if (!/^[0-9a-f]*$/.test(token)) {
    return false;
  }

  const closing = value.slice(MARKER_PREFIX.length + TOKEN_LENGTH);
  return closing === "" || closing === "]";
}

/** The longest tail of `value` that opens either reference form. */
function longestPrefixSuffix(value: string) {
  const maximum = Math.min(
    value.length,
    Math.max(MARKER_PREFIX.length, DIRECTIVE_PREFIX.length) - 1,
  );
  for (let length = maximum; length > 0; length -= 1) {
    const tail = value.slice(-length);
    if (MARKER_PREFIX.startsWith(tail) || DIRECTIVE_PREFIX.startsWith(tail)) {
      return length;
    }
  }
  return 0;
}
