// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";

import { DocumentViewerShell, DocumentViewerState } from "./ViewerPrimitives";

afterEach(cleanup);

it("gives loading and failure states the right live-region semantics", () => {
  const { rerender } = render(
    <DocumentViewerState variant="loading">
      Loading document…
    </DocumentViewerState>,
  );

  expect(screen.getByRole("status")).toHaveAttribute("aria-busy", "true");
  expect(screen.getByText("Loading document…")).toBeVisible();

  rerender(
    <DocumentViewerState variant="error">
      This document could not be loaded.
    </DocumentViewerState>,
  );

  expect(screen.getByRole("alert")).not.toHaveAttribute("aria-busy");
});

it("composes viewer layout classes without losing the shared surface", () => {
  render(
    <DocumentViewerShell data-testid="viewer" className="h-full">
      Content
    </DocumentViewerShell>,
  );

  expect(screen.getByTestId("viewer")).toHaveClass(
    "h-full",
    "bg-background",
    "text-foreground",
    "overflow-hidden",
  );
});
