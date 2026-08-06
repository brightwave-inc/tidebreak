// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { DomainFavicon } from "./DomainFavicon";
import { ToolEntriesList } from "./ToolEntriesList";

afterEach(cleanup);

describe("DomainFavicon", () => {
  it("asks DuckDuckGo for the page's host icon", () => {
    render(<DomainFavicon url="https://www.cnbc.com/2026/07/31/story.html" />);
    const image = screen.getByRole("presentation", { hidden: true });
    expect(image.getAttribute("src")).toBe(
      "https://icons.duckduckgo.com/ip3/cnbc.com.ico",
    );
  });

  it("falls back to the globe when the icon cannot load", () => {
    const { container } = render(
      <DomainFavicon url="https://example.com/page" />,
    );
    const image = container.querySelector("img");
    expect(image).not.toBeNull();
    fireEvent.error(image!);
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("svg")).not.toBeNull();
  });
});

describe("ToolEntriesList link marks", () => {
  it("shows a site favicon for each linked result", () => {
    render(
      <ToolEntriesList
        name="web_search"
        result={{
          tool: "entries",
          entries: [
            {
              kind: "link",
              label: "Why the fund imploded",
              detail: "cnbc.com",
              meta: null,
              mediaType: null,
              targetId: null,
              url: "https://www.cnbc.com/story",
            },
          ],
          failures: [],
          elided: 0,
        }}
      />,
    );
    const image = screen.getByRole("presentation", { hidden: true });
    expect(image.getAttribute("src")).toBe(
      "https://icons.duckduckgo.com/ip3/cnbc.com.ico",
    );
  });
});
