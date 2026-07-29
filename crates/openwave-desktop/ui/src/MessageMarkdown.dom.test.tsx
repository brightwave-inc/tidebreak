// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { AssistantSource } from "./AssistantSources";
import { MessageCitationsProvider } from "./InlineCitation";
import { MessageMarkdown } from "./MessageMarkdown";

afterEach(cleanup);

describe("code block copy", () => {
  it("copies the raw source, not the highlighted markup", async () => {
    const user = userEvent.setup();
    render(
      <MessageMarkdown>{"```ts\nconst x: number = 1;\n```"}</MessageMarkdown>,
    );
    await user.click(screen.getByRole("button", { name: "Copy code" }));
    expect(await window.navigator.clipboard.readText()).toBe(
      "const x: number = 1;\n",
    );
  });
});

const REEF = "0b2b1f2c-9d3e-4a5b-8c7d-6e5f4a3b2c1d";
const TIDE = "3f7c8a91-2b4d-4e6f-9a1b-5c7d8e9f0a1b";
const DANGLING = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";

function source(overrides: Partial<AssistantSource> = {}): AssistantSource {
  return {
    id: REEF,
    ordinal: 2,
    documentId: "doc-reef",
    span: { start: 10, end: 40 },
    excerpt: "The reef spans 2,300 kilometres.",
    heading: "Extent",
    pages: [4],
    bounds: [],
    ...overrides,
  };
}

describe("inline citations", () => {
  // Ordinals skip: a citation dropped past the message's bound leaves a gap, so
  // a badge counted off the rendering would renumber every citation after it.
  const sources = [
    source(),
    source({
      id: TIDE,
      ordinal: 4,
      documentId: "doc-tide",
      excerpt: "Tides run to four metres.",
      heading: "Tides",
      pages: [11, 12],
    }),
  ];

  function renderCited(text: string, onOpenSource = vi.fn()) {
    const view = render(
      <MessageCitationsProvider value={{ sources, onOpenSource }}>
        <MessageMarkdown>{text}</MessageMarkdown>
      </MessageCitationsProvider>,
    );
    return { ...view, onOpenSource };
  }

  it("anchors each citation on the phrase it backs, badged by its ordinal", async () => {
    const user = userEvent.setup();
    const { container, onOpenSource } = renderCited(
      `The reef :cit[is the largest in the world]{citation_id=${REEF}}, ` +
        `and :cit[its tides are extreme]{citation_id=${TIDE}}.`,
    );

    const cited = screen.getAllByRole("button");
    expect(cited.map((button) => button.getAttribute("aria-label"))).toEqual([
      "Citation 2",
      "Citation 4",
    ]);
    expect(cited[0]).toHaveTextContent("is the largest in the world");
    expect(cited[1]).toHaveTextContent("its tides are extreme");
    expect(container.textContent).toContain("The reef is the largest");
    expect(container.textContent).not.toContain(":cit");
    expect(container.textContent).not.toContain("citation_id");

    // Each span carries its own evidence, and opening it hands back the
    // snapshot the phrase was anchored to.
    await user.click(cited[1]!);
    const popover = await screen.findByText("Tides");
    expect(popover.parentElement).toHaveTextContent("Tides run to four metres.");
    expect(popover.parentElement).toHaveTextContent("Pages 11, 12");

    await user.click(screen.getByRole("button", { name: "Open source" }));
    expect(onOpenSource).toHaveBeenCalledWith(sources[1]);
  });

  it("reads as prose when the citation is not one the message carries", () => {
    const { container } = renderCited(
      `Reefs :cit[grow slowly]{citation_id=${DANGLING}} and ` +
        ":cit[tides turn]{citation_id=not-a-uuid}.",
    );

    expect(screen.queryAllByRole("button")).toHaveLength(0);
    expect(container.textContent).toBe("Reefs grow slowly and tides turn.");
  });
});
