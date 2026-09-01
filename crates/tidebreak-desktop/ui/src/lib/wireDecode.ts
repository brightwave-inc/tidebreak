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
 * Both decoders bound every string they will draw. Ids, timestamps, and
 * cursors share the named limits below; the decoders choose their own limits
 * for text, by how each field is rendered:
 *
 * - {@link bounded} and {@link nonEmptyBounded} for a field drawn on one
 *   line, which rejects the control and bidirectional characters that could
 *   redraw or reorder it.
 * - {@link boundedBlock} for a field drawn as a block, where line breaks,
 *   carriage returns, and tabs are structure.
 * - {@link boundedRaw} for verbatim data (blobs, diffs, terminal output)
 *   that a dedicated pane renders as it is, so only the length is checked.
 *
 * The presence-only readers remain for fields that are matched rather than
 * drawn, and for the settings importers that read local files.
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
    withinChars(value, maxChars) &&
    !LINE_FORBIDDEN.test(value)
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
 * rather than a line, so line breaks, carriage returns, and tabs are
 * structure rather than spoofing. A carriage return is admitted because
 * GitHub-authored bodies arrive with CRLF line endings and a block renderer
 * treats it as a space. Everything else {@link bounded} rejects is still
 * rejected: an escape sequence or a bidirectional override in a pane of
 * command output could still redraw or reorder what the reader sees.
 */
export function boundedBlock(
  value: unknown,
  maxChars: number,
): value is string {
  return (
    typeof value === "string" &&
    withinChars(value, maxChars) &&
    !BLOCK_FORBIDDEN.test(value)
  );
}

/**
 * A string within `maxChars` code points, with no character clamp at all.
 *
 * For verbatim data the reader asked to see as it is — file content, diffs,
 * terminal reads, command output — where carriage returns and terminal
 * escapes are part of the payload and the pane that draws it already expects
 * them. The length bound still rejects a payload the server could not have
 * produced.
 */
export function boundedRaw(value: unknown, maxChars: number): value is string {
  return typeof value === "string" && withinChars(value, maxChars);
}

/** Every entry passes {@link bounded}. Empty list allowed. */
export function boundedStringList(
  value: unknown,
  maxChars: number,
): value is string[] {
  return Array.isArray(value) && value.every((item) => bounded(item, maxChars));
}

/**
 * Whether `value` holds at most `maxChars` code points. UTF-16 length is an
 * upper bound on the code point count, so a string that fits by length fits
 * without a scan; only a longer one is counted, and only until it overflows.
 */
function withinChars(value: string, maxChars: number): boolean {
  if (value.length <= maxChars) return true;
  let count = 0;
  for (const _ of value) {
    if (++count > maxChars) return false;
  }
  return true;
}

/**
 * The characters {@link forbiddenPreviewCharacter} names, as one scan: C0 and
 * C1 controls, U+2028/U+2029, the bidirectional embeddings, overrides, and
 * isolates. The block form carves out `\n`, `\r`, and `\t`.
 */
const LINE_FORBIDDEN =
  /[\u0000-\u001f\u007f-\u009f\u2028\u2029\u202a-\u202e\u2066-\u2069]/;
const BLOCK_FORBIDDEN =
  /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f\u2028\u2029\u202a-\u202e\u2066-\u2069]/;

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
