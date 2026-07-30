// @vitest-environment jsdom
import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { renderWithRouter } from "../test/router";
import { useStableSourceNav } from "./SourceNav";
import { usePanelNav } from "./usePanelNav";

afterEach(cleanup);

function Harness() {
  const { openPanel, closePanel, toggleFullscreen } = usePanelNav();
  const sourceNav = useStableSourceNav(openPanel);
  return (
    <div>
      <button onClick={() => openPanel({ type: "folders" })}>open folders</button>
      <button onClick={() => openPanel({ type: "outputs" })}>open outputs</button>
      <button
        onClick={() =>
          sourceNav.openCitation({ documentId: "doc-2", citationId: "cite-1" })
        }
      >
        open citation
      </button>
      <button onClick={() => closePanel("left")}>close left</button>
      <button onClick={() => closePanel("right")}>close right</button>
      <button onClick={() => toggleFullscreen("left")}>fullscreen left</button>
    </div>
  );
}

async function mount(initialUrl: string) {
  return renderWithRouter(<Harness />, { initialUrl });
}

describe("usePanelNav", () => {
  it("opens a navigation panel on the left, leaving the conversation beside it", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1");

    await user.click(screen.getByText("open folders"));

    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "folders", right: "chat" }),
    );
  });

  it("replaces a panel that belongs on the same side", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?left=folders&right=chat");

    await user.click(screen.getByText("open outputs"));

    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "outputs", right: "chat" }),
    );
  });

  // A citation is clicked in the transcript, which in a split layout is one of
  // the two slots. The document has to arrive beside it: replacing the
  // conversation with the source it cites takes away the thing being read.
  it("opens a citation beside the conversation rather than over it", async () => {
    const user = userEvent.setup();
    // Another source is already open, and the conversation is fullscreen — the
    // reader is reading it, and the citation they clicked is in it.
    const { router } = await mount(
      "/c/chat-1?left=chat&right=document.doc-1&fullscreen=left",
    );

    await user.click(screen.getByText("open citation"));

    await waitFor(() =>
      expect(router.state.location.search).toEqual({
        left: "chat",
        right: "document.doc-2.cite-1",
      }),
    );
  });

  it("collapses to the bare URL when the survivor is the conversation", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?left=folders&right=chat");

    await user.click(screen.getByText("close left"));

    await waitFor(() => expect(router.state.location.search).toEqual({}));
  });

  it("returns the survivor to its own side rather than leaving it stranded", async () => {
    const user = userEvent.setup();
    // Folders sits on the right here, which is not where a navigation panel
    // belongs; closing the other slot should move it home.
    const { router } = await mount("/c/chat-1?left=outputs&right=folders");

    await user.click(screen.getByText("close left"));

    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "folders", right: "chat" }),
    );
  });

  it("toggles fullscreen on and back off", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?left=folders&right=chat");

    await user.click(screen.getByText("fullscreen left"));
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({ fullscreen: "left" }),
    );

    await user.click(screen.getByText("fullscreen left"));
    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "folders", right: "chat" }),
    );
  });

  it("does nothing on a conversation with no panels open", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1");

    await user.click(screen.getByText("fullscreen left"));

    expect(router.state.location.search).toEqual({});
  });
});
