// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  PanelBreadcrumb,
  PanelPrimaryHeader,
  PanelSecondaryHeader,
} from "./PanelHeader";

afterEach(cleanup);

describe("PanelPrimaryHeader", () => {
  it("offers only the chrome the host wired up", () => {
    // A panel that cannot be closed — the conversation, say — must not render a
    // dead close button, and neither control should appear by default.
    render(
      <PanelPrimaryHeader
        breadcrumb={<PanelBreadcrumb firstPart="Sources" />}
      />,
    );

    expect(screen.queryByRole("button", { name: "Close" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Fullscreen" })).toBeNull();
  });

  it("reports close and fullscreen to the host", async () => {
    const onClose = vi.fn();
    const onToggleFullscreen = vi.fn();
    render(
      <PanelPrimaryHeader
        onClose={onClose}
        onToggleFullscreen={onToggleFullscreen}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: "Fullscreen" }));
    await userEvent.click(screen.getByRole("button", { name: "Close" }));

    expect(onToggleFullscreen).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("names the fullscreen control for the state it moves to", async () => {
    const onToggleFullscreen = vi.fn();
    render(
      <PanelPrimaryHeader
        isFullscreen
        onToggleFullscreen={onToggleFullscreen}
      />,
    );

    await userEvent.click(
      screen.getByRole("button", { name: "Exit fullscreen" }),
    );

    expect(onToggleFullscreen).toHaveBeenCalledOnce();
  });
});

describe("PanelBreadcrumb", () => {
  it("shows a parent alone until an item is drilled into", () => {
    const { container } = render(<PanelBreadcrumb firstPart="Sources" />);

    expect(screen.getByText("Sources")).toBeTruthy();
    expect(container.textContent).not.toContain("/");
  });

  it("separates the parent from the current item", () => {
    render(
      <PanelBreadcrumb
        firstPart="Sources"
        currentItem="Quarterly report.pdf"
      />,
    );

    expect(screen.getByText("Sources")).toBeTruthy();
    expect(screen.getByText("/")).toBeTruthy();
    expect(screen.getByText("Quarterly report.pdf")).toBeTruthy();
  });
});

describe("PanelSecondaryHeader", () => {
  it("renders its title row content", () => {
    render(
      <PanelSecondaryHeader>
        <h1>Sources</h1>
      </PanelSecondaryHeader>,
    );

    expect(screen.getByRole("heading", { name: "Sources" })).toBeTruthy();
  });
});
