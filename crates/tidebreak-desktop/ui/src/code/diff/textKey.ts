/**
 * A short key for some text: equal texts get equal keys, and different ones
 * collide too rarely to matter. Two 32-bit FNV-1a hashes read in one pass,
 * with the length beside them.
 */
export function textKey(parts: Iterable<string>): string {
  let a = 0x811c9dc5;
  let b = 0x01000193 ^ 0x5bd1e995;
  let length = 0;
  for (const text of parts) {
    for (let index = 0; index < text.length; index += 1) {
      const code = text.charCodeAt(index);
      a = Math.imul(a ^ code, 0x01000193);
      b = Math.imul(b ^ code, 0x5bd1e995) ^ (b >>> 15);
    }
    // A separator no part holds, so "ab" + "c" never reads as "a" + "bc".
    a = Math.imul(a ^ 0xffff, 0x01000193);
    b = Math.imul(b ^ 0xffff, 0x5bd1e995) ^ (b >>> 15);
    length += text.length + 1;
  }
  return `${length}.${(a >>> 0).toString(36)}.${(b >>> 0).toString(36)}`;
}
