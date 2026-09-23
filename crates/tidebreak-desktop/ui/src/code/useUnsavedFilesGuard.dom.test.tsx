// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  Outlet,
  RouterProvider,
} from "@tanstack/react-router";
import { afterEach, describe, expect, it } from "vitest";

import type { LayoutState } from "@/panel/panelTypes";
import {
  panelSearchFrom,
  searchFromLayout,
  type PanelSearch,
} from "@/panel/panelUrl";
import { useLayoutState } from "@/panel/usePanelNav";
import { useCodeFileDraftStore } from "./CodeFileDraftStore";
import {
  openFilePaths,
  unsavedFilesClosedBy,
  useUnsavedFilesGuard,
} from "./useUnsavedFilesGuard";

const HASH = "a".repeat(64);

function twoFiles(): LayoutState {
  return {
    tabs: [
      { type: "file", path: "README.md" },
      { type: "file", path: "docs/setup.md" },
    ],
    activeIndex: 0,
    fullscreen: false,
  };
}

function oneFile(): LayoutState {
  return {
    tabs: [{ type: "file", path: "docs/setup.md" }],
    activeIndex: 0,
    fullscreen: false,
  };
}

function edit(path: string, text: string) {
  act(() => {
    const drafts = useCodeFileDraftStore.getState();
    drafts.startEditing("ws-1", path, { text: "original\n", hash: HASH });
    drafts.setText("ws-1", path, text);
  });
}

function Guarded() {
  const layout = useLayoutState();
  const dialog = useUnsavedFilesGuard("ws-1", layout);
  return (
    <>
      {dialog}
      <p>{[...openFilePaths(layout)].join(", ") || "no files"}</p>
    </>
  );
}

async function workspaceRouter(layout: LayoutState) {
  const root = createRootRoute({ component: () => <Outlet /> });
  const workspace = createRoute({
    getParentRoute: () => root,
    path: "/code/w/$workspaceId",
    validateSearch: (search: Record<string, unknown>): PanelSearch =>
      panelSearchFrom(search),
    component: Guarded,
  });
  const home = createRoute({
    getParentRoute: () => root,
    path: "/code",
    component: () => <p>Code home</p>,
  });
  const search = new URLSearchParams(
    Object.entries(searchFromLayout(layout)).filter(
      (entry): entry is [string, string] => typeof entry[1] === "string",
    ),
  );
  const router = createRouter({
    routeTree: root.addChildren([workspace, home]),
    history: createMemoryHistory({
      initialEntries: [`/code/w/ws-1?${search}`],
    }),
  });
  await router.load();
  render(<RouterProvider router={router} />);
  return router;
}

function closeTo(
  router: Awaited<ReturnType<typeof workspaceRouter>>,
  layout: LayoutState,
) {
  return act(async () => {
    void router.navigate({
      to: "/code/w/$workspaceId",
      params: { workspaceId: "ws-1" },
      search: searchFromLayout(layout),
    });
  });
}

afterEach(() => {
  cleanup();
  useCodeFileDraftStore.setState({ drafts: {} });
});

describe("useUnsavedFilesGuard", () => {
  it("asks before closing a tab with unsaved changes, and keeps it on cancel", async () => {
    const router = await workspaceRouter(twoFiles());
    expect(await screen.findByText("README.md, docs/setup.md")).toBeVisible();
    edit("README.md", "changed\n");

    await closeTo(router, oneFile());
    const dialog = await screen.findByRole("alertdialog");
    expect(
      within(dialog).getByText("Discard unsaved changes to README.md?"),
    ).toBeVisible();
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));

    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(screen.getByText("README.md, docs/setup.md")).toBeVisible();
    expect(
      Object.values(useCodeFileDraftStore.getState().drafts).map(
        (draft) => draft.text,
      ),
    ).toEqual(["changed\n"]);
  });

  it("closes the tab and drops the changes once you choose Discard", async () => {
    const router = await workspaceRouter(twoFiles());
    await screen.findByText("README.md, docs/setup.md");
    edit("README.md", "changed\n");

    await closeTo(router, oneFile());
    const dialog = await screen.findByRole("alertdialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Discard" }));

    expect(await screen.findByText("docs/setup.md")).toBeVisible();
    expect(useCodeFileDraftStore.getState().drafts).toEqual({});
  });

  it("closes a tab without asking when nothing in it is unsaved", async () => {
    const router = await workspaceRouter(twoFiles());
    await screen.findByText("README.md, docs/setup.md");
    edit("docs/setup.md", "changed\n");

    await closeTo(router, {
      tabs: [{ type: "file", path: "docs/setup.md" }],
      activeIndex: 0,
      fullscreen: false,
    });

    expect(await screen.findByText("docs/setup.md")).toBeVisible();
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("asks about every unsaved file before leaving the workspace", async () => {
    const router = await workspaceRouter(twoFiles());
    await screen.findByText("README.md, docs/setup.md");
    edit("README.md", "one\n");
    edit("docs/setup.md", "two\n");

    await act(async () => {
      void router.navigate({ to: "/code" });
    });
    const dialog = await screen.findByRole("alertdialog");
    expect(
      within(dialog).getByText("Discard unsaved changes to 2 files?"),
    ).toBeVisible();
    fireEvent.click(within(dialog).getByRole("button", { name: "Discard" }));

    expect(await screen.findByText("Code home")).toBeVisible();
    expect(useCodeFileDraftStore.getState().drafts).toEqual({});
  });
});

describe("unsavedFilesClosedBy", () => {
  it("keeps a file that moved to the split group", () => {
    const next = searchFromLayout({
      tabs: [],
      activeIndex: 0,
      fullscreen: false,
      editorSplit: {
        tabs: [{ type: "file", path: "README.md" }],
        activeIndex: 0,
      },
    });
    expect(
      unsavedFilesClosedBy("ws-1", ["README.md"], {
        pathname: "/code/w/ws-1",
        search: next,
      }),
    ).toEqual([]);
  });

  it("names every unsaved file when the next page is another workspace", () => {
    expect(
      unsavedFilesClosedBy("ws-1", ["README.md", "a.md"], {
        pathname: "/code/w/ws-2",
        search: searchFromLayout(twoFiles()),
      }),
    ).toEqual(["README.md", "a.md"]);
  });
});
