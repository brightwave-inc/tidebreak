const DIRECTIVE_PREFIX = ":cit[";
const MAX_HELD_DIRECTIVE = 1_024;

/**
 * Keeps partial citation markup out of the live transcript. Once the closing
 * attribute block arrives, only the cited phrase is emitted; terminal hydration
 * replaces it with the durable directive and locator snapshot.
 */
export class AssistantSourceMarkerStreamScrubber {
  private pending = "";

  push(delta: string): string {
    let input = this.pending + delta;
    let output = "";
    this.pending = "";

    while (input.length > 0) {
      const start = input.indexOf(DIRECTIVE_PREFIX);
      if (start === -1) {
        const held = longestPrefixSuffix(input);
        output += input.slice(0, input.length - held);
        this.pending = input.slice(input.length - held);
        break;
      }

      output += input.slice(0, start);
      const candidate = input.slice(start);
      const scanned = scanDirective(candidate);
      if (scanned.kind === "complete") {
        output += scanned.phrase;
        input = candidate.slice(scanned.length);
        continue;
      }
      if (scanned.kind === "partial") {
        this.pending = candidate;
        break;
      }

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

type Scan =
  | { kind: "complete"; phrase: string; length: number }
  | { kind: "partial" }
  | { kind: "invalid" };

function scanDirective(candidate: string): Scan {
  if (candidate.length > MAX_HELD_DIRECTIVE) return { kind: "invalid" };
  const closePhrase = candidate.indexOf("]", DIRECTIVE_PREFIX.length);
  if (closePhrase === -1) return { kind: "partial" };
  if (candidate[closePhrase + 1] === undefined) return { kind: "partial" };
  if (candidate[closePhrase + 1] !== "{") return { kind: "invalid" };
  const closeAttributes = candidate.indexOf("}", closePhrase + 2);
  if (closeAttributes === -1) return { kind: "partial" };
  return {
    kind: "complete",
    phrase: candidate.slice(DIRECTIVE_PREFIX.length, closePhrase),
    length: closeAttributes + 1,
  };
}

function longestPrefixSuffix(value: string) {
  const maximum = Math.min(value.length, DIRECTIVE_PREFIX.length - 1);
  for (let length = maximum; length > 0; length -= 1) {
    if (DIRECTIVE_PREFIX.startsWith(value.slice(-length))) return length;
  }
  return 0;
}
