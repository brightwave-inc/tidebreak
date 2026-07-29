// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { Composer } from "./Composer";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const noop = async () => {};

/**
 * Uploading moved behind a menu, so the trigger is now the only way to reach
 * it. A trigger that fails to open makes attachment unreachable without
 * failing anything else — the static markup test still passes, because the
 * button is right there.
 */
it("opens the add menu and runs the upload it offers", async () => {
  const onAttach = vi.fn(async () => {});
  render(
    <Composer
      activeTurnId={null}
      busy={false}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft=""
      canAttach
      attaching={false}
      attachedSourceName={null}
      attachError={null}
      onAttach={onAttach}
      onDraftChange={vi.fn()}
      onSend={noop}
      onSteer={noop}
      onStop={noop}
      resetKey="chat-1"
      steerError={null}
      steerPending={false}
      steerStatus={null}
    />,
  );

  // Nothing runs from the trigger itself: it opens the menu.
  await userEvent.click(screen.getByRole("button", { name: "Add to this chat" }));
  expect(onAttach).not.toHaveBeenCalled();

  await userEvent.click(await screen.findByRole("menuitem", { name: /Upload files/ }));
  await waitFor(() => expect(onAttach).toHaveBeenCalledTimes(1));
});
