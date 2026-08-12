// @vitest-environment jsdom
import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { renderWithRouter } from "../test/router";
import { useStableSourceNav } from "./SourceNav";
import { usePanelNav } from "./usePanelNav";

afterEach(cleanup);

function Harness() {
  const { layout, openPanel, closeTab, closeAllPanels, toggleFullscreen } = usePanelNav();
  const sourceNav = useStableSourceNav(openPanel);
  return (
    <div>
      <button onClick={() => openPanel({ type: "folders" })}>open folders</button>
      <button onClick={() => openPanel({ type: "outputs" })}>open outputs</button>
      <button onClick={() => openPanel({ type: "outputs", outputId: "out-1" })}>
        open one output
      </button>
      <button
        onClick={() => sourceNav.openCitation({ documentId: "doc-2", citationId: "cite-1" })}
      >
        open citation
      </button>
      <button onClick={() => closeTab()}>close active</button>
      <button onClick={() => closeTab({ type: "folders" })}>close folders</button>
      <button onClick={() => closeAllPanels()}>close all</button>
      <button onClick={() => toggleFullscreen()}>fullscreen</button>
      <output>{`${layout.tabs.length}:${layout.activeIndex}`}</output>
    </div>
  );
}

async function mount(initialUrl: string) {
  return renderWithRouter(<Harness />, { initialUrl });
}

describe("usePanelNav", () => {
  it("appends each newly opened panel and shows it", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1");

    await user.click(screen.getByText("open folders"));
    await waitFor(() => expect(router.state.location.search).toEqual({ tabs: "folders" }));

    await user.click(screen.getByText("open outputs"));
    await waitFor(() =>
      expect(router.state.location.search).toEqual({
        tabs: "folders,outputs",
        active: "outputs",
      }),
    );
  });

  it("brings an open panel forward rather than opening it twice", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?tabs=folders,outputs&active=outputs");

    await user.click(screen.getByText("open folders"));

    await waitFor(() =>
      expect(router.state.location.search).toEqual({ tabs: "folders,outputs" }),
    );
  });

  // A library and the item drilled into from it are one panel showing
  // something else, so opening the detail moves the tab rather than adding one.
  it("updates the tab in place when the panel it holds is addressed again", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?tabs=outputs");

    await user.click(screen.getByText("open one output"));

    await waitFor(() =>
      expect(router.state.location.search).toEqual({ tabs: "outputs.out-1" }),
    );
  });

  it("opens a citation beside the conversation, keeping what was already open", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?tabs=document.doc-1");

    await user.click(screen.getByText("open citation"));

    await waitFor(() =>
      expect(router.state.location.search).toEqual({
        tabs: "document.doc-1,document.doc-2.cite-1",
        active: "document.doc-2.cite-1",
      }),
    );
  });

  it("hands focus to the neighbour on the left when the open tab closes", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?tabs=folders,outputs&active=outputs");

    await user.click(screen.getByText("close active"));

    await waitFor(() => expect(router.state.location.search).toEqual({ tabs: "folders" }));
  });

  it("keeps showing the same panel when a tab beside it closes", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?tabs=folders,outputs&active=outputs");

    await user.click(screen.getByText("close folders"));

    await waitFor(() => expect(router.state.location.search).toEqual({ tabs: "outputs" }));
  });

  it("returns to the conversation alone when the last tab closes", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?tabs=folders");

    await user.click(screen.getByText("close active"));

    await waitFor(() => expect(router.state.location.search).toEqual({}));
  });

  it("closes every panel at once", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?tabs=folders,outputs&fullscreen=1");

    await user.click(screen.getByText("close all"));

    await waitFor(() => expect(router.state.location.search).toEqual({}));
  });

  it("toggles fullscreen on and back off, and not at all with nothing open", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?tabs=folders");

    await user.click(screen.getByText("fullscreen"));
    await waitFor(() =>
      expect(router.state.location.search).toEqual({ tabs: "folders", fullscreen: "1" }),
    );

    await user.click(screen.getByText("fullscreen"));
    await waitFor(() => expect(router.state.location.search).toEqual({ tabs: "folders" }));

    await user.click(screen.getByText("close active"));
    await waitFor(() => expect(router.state.location.search).toEqual({}));
    await user.click(screen.getByText("fullscreen"));
    expect(router.state.location.search).toEqual({});
  });
});
