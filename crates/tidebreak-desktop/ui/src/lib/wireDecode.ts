/**
 * Primitive readers shared by the hand-written decoders over the generated
 * wire types (`api/parsers.ts` and `code/parsers.ts`).
 *
 * Each parser file used to carry its own copy of these, so a fix to one
 * primitive reached one decoder and not the other. Everything here is a type
 * guard over `unknown`: it narrows, it never coerces, and it never throws. A
 * parser that wants to reject a payload composes these into one boolean and
 * returns `null`.
 *
 * Two conventions live side by side on purpose and the names keep them apart:
 *
 * - The code-mode decoder accepts any string and checks presence only
 *   (`nonEmptyString`, `optionalString`, `nullableString`). Its payloads come
 *   from the local server's own snapshots and are rendered through components
 *   that clamp for themselves.
 * - The chat decoder bounds every string it will draw (`bounded`,
 *   `nonEmptyBounded`, `boundedBlock`) and rejects the control and
 *   bidirectional characters that could redraw or reorder a line. Its payloads
 *   carry model- and tool-authored text straight into one-line previews.
 *
 * Moving the code-mode decoder onto the bounded readers is a behavior change
 * with its own review (brightwave-inc/tidebreak#2977), so this module only
 * makes the two conventions visible next to each other.
 */

export { isRecord, onlyKeys } from "./guards";

// ---------------------------------------------------------------------------
// Shared guard limits
// ---------------------------------------------------------------------------

/** Longest opaque identifier the chat decoder accepts (call, turn, chat, run, workspace ids). */
export const MAX_WIRE_ID_CHARS = 128;

/** Longest timestamp string the chat decoder accepts. RFC 3339 needs about 35. */
export const MAX_WIRE_TIMESTAMP_CHARS = 64;

/** Longest opaque pagination cursor the chat decoder accepts. */
export const MAX_WIRE_CURSOR_CHARS = 256;

// ---------------------------------------------------------------------------
// Presence-only string readers (code-mode convention)
// ---------------------------------------------------------------------------

/** A string with at least one character. Whitespace counts. */
export function nonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.length > 0;
}

/** Absent or a string. `null` is rejected: the field is `Option<String>` with `skip_serializing_if`. */
export function optionalString(value: unknown): value is string | undefined {
  return value === undefined || typeof value === "string";
}

/** `null` or a string, empty allowed. `undefined` is rejected: the key must be present. */
export function nullableString(value: unknown): value is string | null {
  return value === null || typeof value === "string";
}

/**
 * `null` or a non-empty string.
 *
 * Differs from {@link nullableString} in rejecting the empty string. The chat
 * decoder uses this for fields the model either set or left `null`, where an
 * empty string means the payload is not the shape it claims to be.
 */
export function nullableNonEmptyString(value: unknown): value is string | null {
  return value === null || (typeof value === "string" && value.length > 0);
}

/** Every entry is a string. Empty list allowed. */
export function isStringList(value: unknown): value is string[] {
  return (
    Array.isArray(value) && value.every((item) => typeof item === "string")
  );
}

// ---------------------------------------------------------------------------
// Bounded string readers (chat convention)
// ---------------------------------------------------------------------------

/**
 * A string within `maxChars` code points and free of any character that could
 * break the one line it is rendered on or spoof its visual order: C0/C1
 * controls, the line and paragraph separators, and the bidirectional
 * overrides and isolates. This mirrors the projection's own clamp
 * (`preview_formatting_character` in `tidebreak-core`), because the renderer
 * validates what it is about to draw rather than trusting that the sender
 * already did.
 *
 * Unlike {@link nonEmptyBounded} an empty string passes. Nothing on this wire
 * is expected to be empty — the projection drops a field that clamps away —
 * so this only avoids rejecting a whole payload over a field whose emptiness
 * says nothing about its trustworthiness.
 */
export function bounded(value: unknown, maxChars: number): value is string {
  return (
    typeof value === "string" &&
    Array.from(value).length <= maxChars &&
    !Array.from(value).some(forbiddenPreviewCharacter)
  );
}

/** {@link bounded}, and not blank once trimmed. */
export function nonEmptyBounded(
  value: unknown,
  maxChars: number,
): value is string {
  return bounded(value, maxChars) && value.trim().length > 0;
}

/**
 * The same clamp as {@link bounded} for a field that is drawn as a block
 * rather than a line, so line breaks and tabs are structure rather than
 * spoofing. Everything else {@link bounded} rejects is still rejected: an
 * escape sequence or a bidirectional override in a pane of command output
 * could still redraw or reorder what the reader sees.
 */
export function boundedBlock(
  value: unknown,
  maxChars: number,
): value is string {
  return (
    typeof value === "string" &&
    Array.from(value).length <= maxChars &&
    !Array.from(value).some(
      (character) =>
        forbiddenPreviewCharacter(character) &&
        character !== "\n" &&
        character !== "\t",
    )
  );
}

/**
 * Whether one code point could break a single rendered line or reorder it:
 * C0 and C1 controls, U+2028/U+2029, the bidirectional embeddings and
 * overrides (U+202A–U+202E), and the bidirectional isolates (U+2066–U+2069).
 */
export function forbiddenPreviewCharacter(character: string): boolean {
  const code = character.codePointAt(0) ?? 0;
  return (
    code < 32 ||
    (code >= 127 && code <= 159) ||
    code === 0x2028 ||
    code === 0x2029 ||
    (code >= 0x202a && code <= 0x202e) ||
    (code >= 0x2066 && code <= 0x2069)
  );
}

/**
 * An absolute `http:` or `https:` URL. Anything the URL parser rejects, and
 * every other scheme, fails: the renderer is the last thing standing between
 * stored text and a browser window.
 */
export function isWebUrl(value: unknown): value is string {
  if (typeof value !== "string") return false;
  try {
    const { protocol } = new URL(value);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// Enum and number readers
// ---------------------------------------------------------------------------

/** A string that is one of a closed vocabulary. */
export function isMember<T extends string>(
  value: unknown,
  allowed: ReadonlySet<T>,
): value is T {
  return typeof value === "string" && allowed.has(value as T);
}

/** A number that is neither `NaN` nor infinite. */
export function isFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

/** A safe integer at or above zero. */
export function isNonNegativeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}

/** A safe integer above zero. */
export function isPositiveInteger(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) > 0;
}
