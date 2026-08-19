// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { FilesPanel } from "./FilesPanel";

afterEach(() => {
  cleanup();
});

const TREE = {
  paths: ["README.md", "new.txt", "src/lib.rs", "src/code/mod.rs"],
  truncated: false,
};

function makeClient(tree = TREE) {
  return {
    listCodeWorkspaceTree: vi
      .fn()
      .mockImplementation((_id: string, opts?: { query?: string }) => {
        const needle = opts?.query?.trim().toLowerCase();
        const paths = needle
          ? tree.paths.filter((path) => path.toLowerCase().includes(needle))
          : tree.paths;
        return Promise.resolve({ paths, truncated: tree.truncated });
      }),
  };
}

describe("FilesPanel", () => {
  it("nests folders and opens a file from the tree", async () => {
    const onOpenFile = vi.fn();
    const client = makeClient();
    render(
      <FilesPanel
        client={client}
        workspaceId="ws-1"
        onOpenFile={onOpenFile}
      />,
    );
    expect(await screen.findByText("README.md")).toBeInTheDocument();
    expect(screen.getByText("src")).toBeInTheDocument();
    expect(screen.getByText("lib.rs")).toBeInTheDocument();
    expect(screen.queryByText("Changes")).not.toBeInTheDocument();
    expect(screen.queryByText("+3")).not.toBeInTheDocument();

    await userEvent.setup().click(screen.getByText("lib.rs"));
    expect(onOpenFile).toHaveBeenCalledWith("src/lib.rs");
    expect(client.listCodeWorkspaceTree).toHaveBeenCalledWith("ws-1", {
      limit: 5000,
    });
  });

  it("collapses a top-level folder", async () => {
    const client = makeClient();
    render(
      <FilesPanel client={client} workspaceId="ws-1" onOpenFile={vi.fn()} />,
    );
    expect(await screen.findByText("lib.rs")).toBeInTheDocument();
    await userEvent.setup().click(screen.getByText("src"));
    expect(screen.queryByText("lib.rs")).not.toBeInTheDocument();
    await userEvent.setup().click(screen.getByText("src"));
    expect(screen.getByText("lib.rs")).toBeInTheDocument();
  });

  it("marks the selected file with aria-current", async () => {
    const client = makeClient();
    render(
      <FilesPanel
        client={client}
        workspaceId="ws-1"
        selected="src/lib.rs"
        onOpenFile={vi.fn()}
      />,
    );
    expect(await screen.findByText("lib.rs")).toBeInTheDocument();
    expect(screen.getByRole("treeitem", { name: "lib.rs" })).toHaveAttribute(
      "aria-current",
      "true",
    );
  });

  it("walks the tree with the arrow keys and opens a file from the keyboard", async () => {
    const onOpenFile = vi.fn();
    const client = makeClient();
    render(
      <FilesPanel client={client} workspaceId="ws-1" onOpenFile={onOpenFile} />,
    );
    expect(await screen.findByText("lib.rs")).toBeInTheDocument();

    // One tab stop for the whole tree: the first row. The arrows do the rest.
    const src = screen.getByRole("treeitem", { name: "src" });
    expect(src).toHaveAttribute("tabindex", "0");
    expect(src).toHaveAttribute("aria-level", "1");
    expect(screen.getByRole("treeitem", { name: "lib.rs" })).toHaveAttribute(
      "aria-level",
      "2",
    );
    src.focus();

    // Left closes an open folder; Right opens it again, and Right from an open
    // folder steps into it.
    fireEvent.keyDown(src, { key: "ArrowLeft" });
    expect(screen.queryByText("lib.rs")).not.toBeInTheDocument();
    expect(src).toHaveAttribute("aria-expanded", "false");
    fireEvent.keyDown(src, { key: "ArrowRight" });
    expect(screen.getByText("lib.rs")).toBeInTheDocument();
    fireEvent.keyDown(src, { key: "ArrowRight" });
    expect(screen.getByRole("treeitem", { name: "code" })).toHaveFocus();

    fireEvent.keyDown(document.activeElement!, { key: "End" });
    const last = screen.getByRole("treeitem", { name: "README.md" });
    expect(last).toHaveFocus();
    fireEvent.keyDown(last, { key: "Enter" });
    expect(onOpenFile).toHaveBeenCalledWith("README.md");

    fireEvent.keyDown(last, { key: "Home" });
    expect(screen.getByRole("treeitem", { name: "src" })).toHaveFocus();
  });

  it("names the search that found nothing and offers the way back", async () => {
    const client = makeClient();
    render(
      <FilesPanel client={client} workspaceId="ws-1" onOpenFile={vi.fn()} />,
    );
    expect(await screen.findByText("README.md")).toBeInTheDocument();
    await userEvent
      .setup()
      .type(screen.getByRole("searchbox", { name: "Search files" }), "zzz");
    expect(await screen.findByText("zzz")).toBeInTheDocument();

    await userEvent
      .setup()
      .click(screen.getByRole("button", { name: "Clear search and filters" }));
    expect(await screen.findByText("README.md")).toBeInTheDocument();
  });

  it("focuses search on Cmd+F and filters by include and exclude", async () => {
    const client = makeClient();
    render(
      <FilesPanel client={client} workspaceId="ws-1" onOpenFile={vi.fn()} />,
    );
    expect(await screen.findByText("README.md")).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "f", metaKey: true });
    const search = screen.getByRole("searchbox", { name: "Search files" });
    expect(search).toHaveFocus();
    const include = screen.getByRole("textbox", { name: "Files to include" });
    await userEvent.setup().type(include, "*.rs");
    expect(screen.getByText("lib.rs")).toBeInTheDocument();
    expect(screen.queryByText("README.md")).not.toBeInTheDocument();
    expect(screen.queryByText("new.txt")).not.toBeInTheDocument();

    const exclude = screen.getByRole("textbox", { name: "Files to exclude" });
    await userEvent.setup().clear(include);
    await userEvent.setup().type(exclude, "*.md");
    expect(screen.queryByText("README.md")).not.toBeInTheDocument();
    expect(screen.getByText("new.txt")).toBeInTheDocument();
  });
});
