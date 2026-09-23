// @vitest-environment jsdom
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { PrBranchLine } from "./MiddleTruncate";

describe("PrBranchLine", () => {
  it("keeps the base intact and writes GitHub's base ← head order", () => {
    render(
      <PrBranchLine number={2248} base="main" head="thet/ui-pane-redesign" />,
    );
    const line = screen.getByTitle("#2248 · main ← thet/ui-pane-redesign");
    expect(line).toHaveTextContent("#2248 ·");
    expect(line).toHaveTextContent("main");
    expect(line).toHaveTextContent("←");
    expect(line).toHaveTextContent("thet/ui-pane-redesign");
  });
});
