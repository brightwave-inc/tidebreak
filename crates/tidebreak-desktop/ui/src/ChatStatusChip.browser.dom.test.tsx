// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { ChatStatusChip } from "./ChatStatusChip";

afterEach(cleanup);

function baseProps() {
  return {
    outputCount: 0,
    folders: [] as const,
    runs: [] as const,
    onOpenOutputs: vi.fn(),
    onOpenFolders: vi.fn(),
    onOpenPermissions: vi.fn(),
    onOpenAgents: vi.fn(),
  };
}

describe("ChatStatusChip browser row", () => {
  it("hides the browser row when no onOpenBrowser is passed", () => {
    render(<ChatStatusChip {...baseProps()} />);
    expect(
      screen.queryByRole("button", { name: /Browser/ }),
    ).not.toBeInTheDocument();
  });

  it("shows the browser row when onOpenBrowser is passed", () => {
    const onOpenBrowser = vi.fn();
    render(<ChatStatusChip {...baseProps()} onOpenBrowser={onOpenBrowser} />);

    const browserButton = screen.getByRole("button", { name: /Browser/ });
    expect(browserButton).toBeInTheDocument();
  });

  it("calls onOpenBrowser when the row is clicked", () => {
    const onOpenBrowser = vi.fn();
    render(<ChatStatusChip {...baseProps()} onOpenBrowser={onOpenBrowser} />);

    fireEvent.click(screen.getByRole("button", { name: /Browser/ }));
    expect(onOpenBrowser).toHaveBeenCalledOnce();
  });

  it("displays 'Open shared tab' as the summary", () => {
    render(<ChatStatusChip {...baseProps()} onOpenBrowser={vi.fn()} />);

    expect(
      screen.getByRole("button", { name: /Browser.*Open shared tab/ }),
    ).toBeInTheDocument();
  });
});
