const MARKER_PREFIX = "[[ow-source:";
const TOKEN_LENGTH = 32;
const MARKER_LENGTH = MARKER_PREFIX.length + TOKEN_LENGTH + 2;

/**
 * Removes private source markers from streamed assistant text before it is
 * rendered. Only the short tail that could still become a valid marker is
 * retained between deltas; call finish when a stream ends or is interrupted
 * to recover an incomplete marker as literal prose.
 */
export class AssistantSourceMarkerStreamScrubber {
  private pending = "";

  push(delta: string): string {
    let input = this.pending + delta;
    let output = "";
    this.pending = "";

    while (input.length > 0) {
      const markerStart = input.indexOf(MARKER_PREFIX);
      if (markerStart === -1) {
        const pendingLength = longestPrefixSuffix(input);
        output += input.slice(0, input.length - pendingLength);
        this.pending = input.slice(input.length - pendingLength);
        break;
      }

      output += input.slice(0, markerStart);
      const candidate = input.slice(markerStart);
      if (candidate.length >= MARKER_LENGTH) {
        const possibleMarker = candidate.slice(0, MARKER_LENGTH);
        if (isSourceMarker(possibleMarker)) {
          input = candidate.slice(MARKER_LENGTH);
          continue;
        }
      } else if (couldBecomeSourceMarker(candidate)) {
        this.pending = candidate;
        break;
      }

      // This looked like a marker prefix but cannot become a valid marker.
      // Release one character and rescan so malformed prose is preserved and
      // a valid marker beginning later in the same input is still detected.
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

function longestPrefixSuffix(value: string) {
  const maximum = Math.min(value.length, MARKER_PREFIX.length - 1);
  for (let length = maximum; length > 0; length -= 1) {
    if (MARKER_PREFIX.startsWith(value.slice(-length))) {
      return length;
    }
  }
  return 0;
}
