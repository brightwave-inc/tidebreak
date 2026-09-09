// @vitest-environment jsdom
import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const reducedMotion = vi.hoisted(() => ({ current: false as boolean | null }));

vi.mock("motion/react", async (importOriginal) => {
  const mod = await importOriginal<typeof import("motion/react")>();
  return {
    ...mod,
    useReducedMotion: () => reducedMotion.current,
  };
});

import { Loader, resetCometSharedClockForTests } from "./loader";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  reducedMotion.current = false;
  resetCometSharedClockForTests();
  setVisibility("visible");
});

beforeEach(() => {
  reducedMotion.current = false;
  resetCometSharedClockForTests();
  setVisibility("visible");
});

function spinNode(container: HTMLElement) {
  return container.querySelector("[data-comet-spin]");
}

function setVisibility(state: DocumentVisibilityState) {
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    get: () => state,
  });
  Object.defineProperty(document, "hidden", {
    configurable: true,
    get: () => state === "hidden",
  });
  document.dispatchEvent(new Event("visibilitychange"));
}

describe("Loader comet", () => {
  it("phases rotation onto a shared clock so late mounts stay in sync", () => {
    vi.spyOn(performance, "now").mockReturnValue(250);
    const first = render(<Loader variant="comet" speed={1} decorative />);
    expect(spinNode(first.container)).toHaveStyle({ animationDelay: "-250ms" });

    vi.spyOn(performance, "now").mockReturnValue(1250);
    const second = render(<Loader variant="comet" speed={1} decorative />);
    expect(spinNode(second.container)).toHaveStyle({
      animationDelay: "-250ms",
    });

    vi.spyOn(performance, "now").mockReturnValue(600);
    const third = render(<Loader variant="comet" speed={1} decorative />);
    expect(spinNode(third.container)).toHaveStyle({ animationDelay: "-600ms" });
  });

  it("does not restart the cycle when a parent re-renders", () => {
    vi.spyOn(performance, "now").mockReturnValue(250);
    const view = render(<Loader variant="comet" speed={1} decorative />);
    expect(spinNode(view.container)).toHaveStyle({ animationDelay: "-250ms" });

    vi.spyOn(performance, "now").mockReturnValue(800);
    view.rerender(<Loader variant="comet" speed={1} decorative />);
    expect(spinNode(view.container)).toHaveStyle({ animationDelay: "-250ms" });
  });

  it("freezes the shared clock while the document is hidden", () => {
    vi.spyOn(performance, "now").mockReturnValue(400);
    const first = render(<Loader variant="comet" speed={1} decorative />);
    expect(spinNode(first.container)).toHaveStyle({ animationDelay: "-400ms" });

    setVisibility("hidden");
    // Wall clock advances while hidden; phase must not.
    vi.spyOn(performance, "now").mockReturnValue(9400);
    const whileHidden = render(<Loader variant="comet" speed={1} decorative />);
    expect(spinNode(whileHidden.container)).toHaveStyle({
      animationDelay: "-400ms",
    });

    setVisibility("visible");
    vi.spyOn(performance, "now").mockReturnValue(9600);
    const afterResume = render(<Loader variant="comet" speed={1} decorative />);
    // 400ms frozen + 200ms after resume.
    expect(spinNode(afterResume.container)).toHaveStyle({
      animationDelay: "-600ms",
    });
  });

  it("omits the CSS spin node when motion is reduced", () => {
    reducedMotion.current = true;
    const view = render(<Loader variant="comet" speed={1} decorative />);
    expect(spinNode(view.container)).toBeNull();
  });
});
