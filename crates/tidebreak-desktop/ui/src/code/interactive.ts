/**
 * The three class strings every code-mode control shares.
 *
 * Hover and focus are the only two states a supervision surface has to get
 * right on every row, and writing them out per control is how they drift: one
 * row rings in `--ring`, the next in a border colour, a third has no keyboard
 * state at all. These are token-derived and identical everywhere, so a control
 * that reads as clickable also reads as focusable.
 */

/** Focus ring for controls with room around them: cards, chips, tabs, pills. */
export const FOCUS_RING =
  "ring-offset-background focus-visible:ring-ring focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:outline-none";

/**
 * Focus ring for dense rows — transcript tool lines, tree rows, diff headers —
 * where the two-pixel offset would collide with the row above.
 */
export const FOCUS_RING_TIGHT =
  "ring-offset-background focus-visible:ring-ring focus-visible:ring-2 focus-visible:ring-offset-0 focus-visible:outline-none";

/**
 * Focus ring for full-width rail rows. Any ring outside the box reads as a
 * glowing border around the card; an inset hairline marks the row without
 * outlining it.
 */
export const FOCUS_RING_INSET =
  "ring-inset focus-visible:ring-ring/60 focus-visible:ring-1 focus-visible:outline-none";

/**
 * The hover colour change, at the reveal tempo and off under reduced motion.
 *
 * `motion-safe` rather than `motion-reduce:transition-none` because the tint
 * itself is the state; only its easing is decoration.
 */
export const HOVER_TINT =
  "motion-safe:transition-colors motion-safe:duration-[140ms] motion-safe:ease-out";
