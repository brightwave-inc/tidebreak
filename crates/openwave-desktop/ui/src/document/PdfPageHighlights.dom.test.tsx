// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import type { CitationPageBounds } from "@/api";
import { highlightBoxStyle, PdfPageHighlights } from "./PdfPageHighlights";

afterEach(cleanup);

beforeAll(() => {
  // jsdom does not scroll, and the overlay reveals its first box on mount.
  Element.prototype.scrollIntoView = vi.fn();
});

function rect(page: number, top: number): CitationPageBounds {
  return { page, bounds: { left: 1_000, top, width: 5_000, height: 400 } };
}

describe("PdfPageHighlights", () => {
  it("offers the rest of a passage that runs onto a later page", async () => {
    const user = userEvent.setup();
    const onNavigate = vi.fn();
    render(
      <PdfPageHighlights
        page={4}
        // The passage resumes on 6, not on the page immediately after this one.
        highlights={[rect(4, 8_000), rect(6, 500)]}
        onNavigate={onNavigate}
      />,
    );

    await user.click(screen.getByRole("button", { name: /Continues on next page/ }));
    expect(onNavigate).toHaveBeenCalledWith(6);
  });

  it("says nothing about continuing where the passage ends on this page", () => {
    render(
      <PdfPageHighlights page={4} highlights={[rect(4, 8_000)]} onNavigate={vi.fn()} />,
    );

    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });
});

describe("highlightBoxStyle", () => {
  // Ten-thousandths of the page box, drawn as a fraction of the rendered page.
  it("places a rectangle as a percentage of the page it was measured against", () => {
    expect(
      highlightBoxStyle({ left: 1_250, top: 2_500, width: 5_000, height: 400 }),
    ).toEqual({
      left: "calc(12.5% - 2px)",
      top: "calc(25% - 2px)",
      width: "calc(50% + 7px)",
      height: "calc(4% + 7px)",
    });
  });
});
