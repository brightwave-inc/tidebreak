// @vitest-environment jsdom
import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import { Composer } from "./Composer";

function HistoryComposer() {
  const [draft, setDraft] = useState("unfinished draft");
  return (
    <Composer
      activeTurnId={null}
      busy={false}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft={draft}
      history={["newest message", "older message"]}
      onDraftChange={setDraft}
      onSend={async () => undefined}
      onSteer={async () => undefined}
      onStop={async () => undefined}
      resetKey="chat-1"
      steerError={null}
      steerPending={false}
      steerStatus={null}
    />
  );
}

describe("Composer history", () => {
  it("walks older messages and restores the saved draft", async () => {
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      callback(0);
      return 1;
    });
    const user = userEvent.setup();
    render(<HistoryComposer />);
    const field = screen.getByRole("textbox", {
      name: "Message",
    }) as HTMLTextAreaElement;

    field.focus();
    field.setSelectionRange(0, 0);
    await user.keyboard("{ArrowUp}");
    expect(field).toHaveValue("newest message");

    await act(async () => field.setSelectionRange(0, 0));
    await user.keyboard("{ArrowUp}");
    expect(field).toHaveValue("older message");

    await act(async () =>
      field.setSelectionRange(field.value.length, field.value.length),
    );
    await user.keyboard("{ArrowDown}");
    expect(field).toHaveValue("newest message");

    await act(async () =>
      field.setSelectionRange(field.value.length, field.value.length),
    );
    await user.keyboard("{ArrowDown}");
    expect(field).toHaveValue("unfinished draft");
  });
});
