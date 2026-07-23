// Shared vitest setup: registers Testing Library's jest-dom matchers
// (toBeDisabled, toHaveTextContent, …) for every test file. Harmless in the
// node environment; meaningful in files that opt into jsdom.
import "@testing-library/jest-dom/vitest";

// jsdom implements neither scrolling API; components calling scrollTo would
// otherwise throw inside effects. A no-op keeps scroll-follow logic testable.
if (
  typeof Element !== "undefined" &&
  typeof Element.prototype.scrollTo !== "function"
) {
  Element.prototype.scrollTo = () => {};
}
