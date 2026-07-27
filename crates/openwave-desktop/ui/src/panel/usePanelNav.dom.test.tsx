// @vitest-environment jsdom
import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { renderWithRouter } from "../test/router";
import { usePanelNav } from "./usePanelNav";

afterEach(cleanup);

function Harness() {
  const { openPanel, closePanel, toggleFullscreen } = usePanelNav();
  return (
    <div>
      <button onClick={() => openPanel({ type: "sources" })}>open sources</button>
      <button onClick={() => openPanel({ type: "outputs" })}>open outputs</button>
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

    await user.click(screen.getByText("open sources"));

    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "sources", right: "chat" }),
    );
  });

  it("replaces a panel that belongs on the same side", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?left=sources&right=chat");

    await user.click(screen.getByText("open outputs"));

    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "outputs", right: "chat" }),
    );
  });

  it("collapses to the bare URL when the survivor is the conversation", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?left=sources&right=chat");

    await user.click(screen.getByText("close left"));

    await waitFor(() => expect(router.state.location.search).toEqual({}));
  });

  it("returns the survivor to its own side rather than leaving it stranded", async () => {
    const user = userEvent.setup();
    // Sources sits on the right here, which is not where a navigation panel
    // belongs; closing the other slot should move it home.
    const { router } = await mount("/c/chat-1?left=outputs&right=sources");

    await user.click(screen.getByText("close left"));

    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "sources", right: "chat" }),
    );
  });

  it("toggles fullscreen on and back off", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1?left=sources&right=chat");

    await user.click(screen.getByText("fullscreen left"));
    await waitFor(() =>
      expect(router.state.location.search).toMatchObject({ fullscreen: "left" }),
    );

    await user.click(screen.getByText("fullscreen left"));
    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "sources", right: "chat" }),
    );
  });

  it("does nothing on a conversation with no panels open", async () => {
    const user = userEvent.setup();
    const { router } = await mount("/c/chat-1");

    await user.click(screen.getByText("fullscreen left"));

    expect(router.state.location.search).toEqual({});
  });
});
