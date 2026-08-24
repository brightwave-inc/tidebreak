/**
 * The one fuzzy matcher the keyboard surfaces rank with.
 *
 * The file picker and the command palette are the same interaction over
 * different rows — type a few letters, take the first hit — so they score the
 * same way. Two scorers would mean `wsp` finding a file and missing the
 * workspace command named after it, and nobody would be able to say why.
 *
 * Scores are comparable only within one call site's candidate set: they carry
 * a length penalty, so a short label and a long path are not on one scale.
 */

/**
 * How well `target` matches a single `token`, or `-1` for no match at all.
 *
 * Exact beats prefix beats substring beats scattered letters, and a letter
 * landing on a word boundary counts for more than one in the middle — `cpr`
 * should find `create-pr` ahead of anything that merely contains those three
 * letters in order.
 */
export function fuzzyTokenScore(target: string, token: string): number {
  if (target === token) return 2000;
  const contiguous = target.indexOf(token);
  if (contiguous >= 0) {
    return 1200 - contiguous * 8 - (target.length - token.length);
  }
  let cursor = 0;
  let score = 0;
  let previous = -2;
  for (const char of token) {
    const index = target.indexOf(char, cursor);
    if (index < 0) return -1;
    score += index === previous + 1 ? 24 : Math.max(2, 14 - index);
    if (index === 0 || /[-_.]/.test(target[index - 1] ?? "")) score += 16;
    previous = index;
    cursor = index + 1;
  }
  return score - target.length;
}

/**
 * How well `target` matches a whole query, or `-1` when any word misses.
 *
 * Words are scored independently and added, so the query reads as a set of
 * things that must all be true rather than a phrase — "pr merge" finds the
 * merge command whichever order the row happens to name them in. An empty
 * query matches everything at zero, which leaves the caller's own ordering
 * intact.
 */
export function fuzzyScore(target: string, query: string): number {
  const tokens = queryTokens(query);
  if (tokens.length === 0) return 0;
  const haystack = target.toLocaleLowerCase();
  let total = 0;
  for (const token of tokens) {
    const score = fuzzyTokenScore(haystack, token);
    if (score < 0) return -1;
    total += score;
  }
  return total;
}

/** The lowercased words of a query, with the whitespace thrown away. */
export function queryTokens(query: string): string[] {
  return query.trim().toLocaleLowerCase().split(/\s+/).filter(Boolean);
}
