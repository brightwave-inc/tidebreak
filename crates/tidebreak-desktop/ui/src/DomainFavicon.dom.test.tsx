// @vitest-environment jsdom
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { DomainFavicon } from "./DomainFavicon";
import { ToolEntriesList } from "./ToolEntriesList";

afterEach(cleanup);

describe("DomainFavicon", () => {
  it("uses a local globe without issuing a third-party image request", () => {
    const { container } = render(
      <DomainFavicon url="https://example.com/page" />,
    );
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("svg")).not.toBeNull();
  });
});

describe("ToolEntriesList link marks", () => {
  it("shows a local source mark for each linked result", () => {
    const { container } = render(
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
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("svg")).not.toBeNull();
  });
});
