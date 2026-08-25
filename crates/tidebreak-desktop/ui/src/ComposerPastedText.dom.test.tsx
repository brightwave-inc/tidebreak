// @vitest-environment jsdom
import { useState } from "react";
import {
  cleanup,
  createEvent,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { Composer } from "./Composer";
import type { PastedTextAttachment } from "./PastedText";

afterEach(cleanup);

function pasteText(target: Element, text: string) {
  const event = createEvent.paste(target, {
    clipboardData: {
      files: [],
      getData: (type: string) => (type === "text/plain" ? text : ""),
    },
  });
  fireEvent(target, event);
  return event;
}

function ComposerHarness({ onSend = vi.fn() }: { onSend?: () => void }) {
  const [draft, setDraft] = useState("");
  const [pastedTexts, setPastedTexts] = useState<PastedTextAttachment[]>([]);
  return (
    <Composer
      activeTurnId={null}
      busy={false}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft={draft}
      pastedTexts={{
        items: pastedTexts,
        onPaste: (text) =>
          setPastedTexts((current) => [
            ...current,
            { id: `paste-${current.length + 1}`, text },
          ]),
        onRemove: (id) =>
          setPastedTexts((current) => current.filter((item) => item.id !== id)),
      }}
      onDraftChange={setDraft}
      onSend={async () => onSend()}
      onSteer={vi.fn(async () => {})}
      onStop={vi.fn(async () => {})}
      resetKey="chat-1"
      steerError={null}
      steerPending={false}
      steerStatus={null}
    />
  );
}

it("holds a long paste outside the textarea and lets it send", async () => {
  const user = userEvent.setup();
  const onSend = vi.fn();
  render(<ComposerHarness onSend={onSend} />);
  const field = screen.getByRole("textbox", { name: "Message" });
  const text = `First source line\n${"x".repeat(1_000)}`;

  const event = pasteText(field, text);

  expect(event.defaultPrevented).toBe(true);
  expect(field).toHaveValue("");
  expect(screen.getByText("Pasted text")).toBeInTheDocument();
  expect(screen.getByText(/First source line/)).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Send message" }));
  expect(onSend).toHaveBeenCalledTimes(1);
});

it("removes held paste context without changing the draft", async () => {
  const user = userEvent.setup();
  render(<ComposerHarness />);
  const field = screen.getByRole("textbox", { name: "Message" });
  await user.type(field, "Keep this instruction");
  pasteText(field, "x".repeat(1_000));

  await user.click(screen.getByRole("button", { name: "Remove pasted text" }));

  expect(field).toHaveValue("Keep this instruction");
  expect(screen.queryByText("Pasted text")).toBeNull();
});
