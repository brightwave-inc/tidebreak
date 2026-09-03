// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import type { Attention } from "../api/types";
import { AttentionBadge } from "./AttentionBadge";

afterEach(() => {
  cleanup();
});

const cases: Array<{ attention: Attention; label: string; type: string }> = [
  {
    attention: {
      state: {
        type: "needs_you",
        prompt: "an approval is waiting",
        source: "structured",
      },
      source: "structured",
    },
    label: "an approval is waiting",
    type: "needs_you",
  },
  {
    attention: {
      state: { type: "stalled", idle_secs: 90 },
      source: "heuristic",
    },
    label: "Stalled",
    type: "stalled",
  },
  {
    attention: {
      state: { type: "fenced", reason: { type: "orphan_alive" } },
      source: "lifecycle",
    },
    label: "Fenced",
    type: "fenced",
  },
  {
    attention: { state: { type: "done_unreviewed" }, source: "lifecycle" },
    label: "Done",
    type: "done_unreviewed",
  },
  {
    attention: {
      state: { type: "manual", note: "look later" },
      source: "user",
    },
    label: "look later",
    type: "manual",
  },
];

describe("AttentionBadge", () => {
  it("renders no pill for Working", () => {
    const { container } = render(
      <AttentionBadge
        attention={{ state: { type: "working" }, source: "lifecycle" }}
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  // A compact mark is often a row's only state. Working drawing as nothing here
  // made a busy agent look idle, which is the one confusion worth a test.
  it("renders a comet loader for Working when compact", () => {
    render(
      <AttentionBadge
        compact
        attention={{ state: { type: "working" }, source: "lifecycle" }}
      />,
    );
    const mark = screen.getByLabelText("Working");
    expect(mark).toHaveAttribute("data-attention", "working");
    expect(mark.querySelector("[data-loader-variant='comet']")).not.toBeNull();
  });

  it("renders no mark for Idle", () => {
    const { container } = render(
      <AttentionBadge
        compact
        attention={{ state: { type: "idle" }, source: "lifecycle" }}
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it.each(cases)(
    "renders $type with its label",
    ({ attention, label, type }) => {
      render(<AttentionBadge attention={attention} />);
      const badge = screen.getByLabelText(label);
      expect(badge).toHaveAttribute("data-attention", type);
    },
  );

  it("uses a compact icon with the same label", () => {
    render(
      <AttentionBadge
        compact
        attention={{
          state: {
            type: "needs_you",
            prompt: "an approval is waiting",
            source: "structured",
          },
          source: "structured",
        }}
      />,
    );
    expect(screen.getByLabelText("an approval is waiting")).toHaveAttribute(
      "data-attention",
      "needs_you",
    );
  });
});
