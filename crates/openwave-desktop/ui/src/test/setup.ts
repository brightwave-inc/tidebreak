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

// jsdom has no ResizeObserver. A stub that never reports a size is as good as
// no stub for anything that lays out from measurements, so this one answers
// once per observed element with a plausible viewport-sized box.
if (typeof globalThis.ResizeObserver === "undefined") {
  globalThis.ResizeObserver = class {
    constructor(private readonly callback: ResizeObserverCallback) {}
    observe(target: Element) {
      const entry = {
        target,
        contentRect: { width: 1024, height: 768, top: 0, left: 0, right: 1024, bottom: 768, x: 0, y: 0 },
        borderBoxSize: [{ inlineSize: 1024, blockSize: 768 }],
        contentBoxSize: [{ inlineSize: 1024, blockSize: 768 }],
        devicePixelContentBoxSize: [{ inlineSize: 1024, blockSize: 768 }],
      } as unknown as ResizeObserverEntry;
      this.callback([entry], this as unknown as ResizeObserver);
    }
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
}
