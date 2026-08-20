// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AssistantSources, type AssistantSource } from "./AssistantSources";

afterEach(cleanup);

function source(
  ordinal: number,
  locator: AssistantSource["locator"] = { kind: "document" },
): AssistantSource {
  return {
    id: `citation-${ordinal}`,
    ordinal,
    documentId: `document-${ordinal}`,
    locator,
  };
}

function openSources() {
  fireEvent.click(screen.getByRole("button", { name: /sources?/i }));
}

describe("AssistantSources", () => {
  it("renders nothing for an empty source set", () => {
    expect(render(<AssistantSources sources={[]} />).container).toBeEmptyDOMElement();
  });

  it("orders and labels lightweight locators, and opens the selected document", () => {
    const onOpenSource = vi.fn();
    const second = source(2, { kind: "lines", start: 12, end: 18 });
    render(
      <AssistantSources
        sources={[second, source(1, { kind: "page", page: 4 })]}
        onOpenSource={onOpenSource}
      />,
    );

    openSources();
    expect(screen.getAllByRole("listitem").map((item) => item.textContent)).toEqual([
      "1Page 4",
      "2Lines 12–18",
    ]);
    fireEvent.click(screen.getByRole("button", { name: "Open source 2" }));
    expect(onOpenSource).toHaveBeenCalledWith(second);
  });

  it("shows a workbook sheet and optional cell range", () => {
    render(
      <AssistantSources
        sources={[
          source(1, { kind: "sheet", sheet: "Revenue", cells: "B2:D9" }),
        ]}
      />,
    );
    openSources();
    expect(screen.getByText("Sheet Revenue · B2:D9")).toBeInTheDocument();
  });
});
