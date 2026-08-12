// @vitest-environment jsdom

import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { CodeViewer, codeLanguageForMediaType } from "./CodeViewer";

describe("CodeViewer", () => {
  it("maps curated media types to highlight languages", () => {
    expect(codeLanguageForMediaType("application/json")).toBe("json");
    expect(codeLanguageForMediaType("text/html")).toBe("xml");
    expect(codeLanguageForMediaType("text/plain")).toBe("plaintext");
  });

  it("highlights JSON without treating it as markdown prose", () => {
    render(
      <CodeViewer
        mediaType="application/json"
        content={'{\n  "title": "Findings"\n}'}
      />,
    );
    expect(screen.queryByRole("heading", { name: "Findings" })).toBeNull();
    expect(screen.getByText('"title"')).toBeVisible();
    expect(document.querySelector(".hljs-string")).not.toBeNull();
  });

  it("keeps content that contains fence markers intact", () => {
    const content = "before\n```\nstill source\n```\nafter";
    render(<CodeViewer mediaType="text/plain" content={content} />);
    expect(screen.getByText(/still source/)).toBeVisible();
    expect(screen.getByText(/before/)).toBeVisible();
    expect(screen.getByText(/after/)).toBeVisible();
  });
});
