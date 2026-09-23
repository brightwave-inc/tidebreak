// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  Composer,
  shouldSteerComposerKey,
  shouldSubmitComposerKey,
} from "./Composer";
import { COMPOSER_KEYS, shouldStopTurnKey } from "./ComposerKeys";
import type { SlashOption } from "./ComposerSlash";

afterEach(() => {
  cleanup();
  document.body.innerHTML = "";
});

const SKILL: SlashOption = {
  kind: "skill",
  name: "pptx",
  label: "Pptx",
  description: "Builds slide decks.",
};

function RunningComposer({
  onStop,
  running = true,
  cancelPending = false,
}: {
  onStop: () => Promise<void>;
  running?: boolean;
  cancelPending?: boolean;
}) {
  const [draft, setDraft] = useState("");
  return (
    <Composer
      activeTurnId={running ? "turn-1" : null}
      busy={running}
      cancelError={null}
      cancelPending={cancelPending}
      disabled={false}
      draft={draft}
      slash={{
        options: [SKILL],
        invoked: [],
        onInvoke: vi.fn(),
        onRemove: vi.fn(),
        loadPromptBody: vi.fn(async () => ""),
        onCommand: vi.fn(),
      }}
      onDraftChange={setDraft}
      onSend={vi.fn(async () => {})}
      onSteer={vi.fn(async () => {})}
      onStop={onStop}
      resetKey="chat-1"
      steerError={null}
      steerPending={false}
      steerStatus={null}
    />
  );
}

function box() {
  return screen.getByRole("textbox", { name: "Message" });
}

describe("Escape in the composer", () => {
  it("stops the running turn, the same as the stop button", () => {
    const onStop = vi.fn(async () => {});
    render(<RunningComposer onStop={onStop} />);

    const handled = !fireEvent.keyDown(box(), { key: "Escape" });

    expect(onStop).toHaveBeenCalledTimes(1);
    expect(handled).toBe(true);
  });

  it("does nothing when no turn is running", () => {
    const onStop = vi.fn(async () => {});
    render(<RunningComposer onStop={onStop} running={false} />);

    fireEvent.keyDown(box(), { key: "Escape" });

    expect(onStop).not.toHaveBeenCalled();
  });

  it("does not ask again while a stop is already on its way", () => {
    const onStop = vi.fn(async () => {});
    render(<RunningComposer onStop={onStop} cancelPending />);

    fireEvent.keyDown(box(), { key: "Escape" });

    expect(onStop).not.toHaveBeenCalled();
  });

  it("closes the slash list first, and stops only on the next press", async () => {
    const user = userEvent.setup();
    const onStop = vi.fn(async () => {});
    render(<RunningComposer onStop={onStop} />);

    await user.click(box());
    await user.keyboard("/");
    expect(screen.getByRole("listbox")).toBeTruthy();

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("listbox")).toBeNull();
    expect(onStop).not.toHaveBeenCalled();

    await user.keyboard("{Escape}");
    expect(onStop).toHaveBeenCalledTimes(1);
  });

  it("leaves Escape to a menu or dialog that is open", () => {
    const onStop = vi.fn(async () => {});
    render(<RunningComposer onStop={onStop} />);
    const menu = document.createElement("div");
    menu.setAttribute("role", "menu");
    menu.setAttribute("data-state", "open");
    document.body.append(menu);

    fireEvent.keyDown(box(), { key: "Escape" });

    expect(onStop).not.toHaveBeenCalled();
  });

  it("leaves Escape to a layer that already took it", () => {
    // Radix layers close on Escape from a capture listener on the document,
    // and mark the key handled before the field sees it.
    const onStop = vi.fn(async () => {});
    render(<RunningComposer onStop={onStop} />);
    const takeEscape = (event: KeyboardEvent) => event.preventDefault();
    document.addEventListener("keydown", takeEscape, { capture: true });
    try {
      fireEvent.keyDown(box(), { key: "Escape" });
    } finally {
      document.removeEventListener("keydown", takeEscape, { capture: true });
    }

    expect(onStop).not.toHaveBeenCalled();
  });

  it("leaves Escape to an input method that is composing", () => {
    const onStop = vi.fn(async () => {});
    render(<RunningComposer onStop={onStop} />);

    fireEvent.keyDown(box(), { key: "Escape", isComposing: true });
    fireEvent.keyDown(box(), { key: "Escape", keyCode: 229 });
    fireEvent.keyDown(box(), { key: "Escape", shiftKey: true });

    expect(onStop).not.toHaveBeenCalled();
  });
});

describe("composer keys in the shortcuts dialog", () => {
  it("lists the keys the composer actually answers", () => {
    // The dialog reads these rows, and the composer matches keys with its own
    // predicates. Each row that names a predicate has to satisfy it, or the
    // dialog would describe a key that does something else.
    const press = (key: string, modifiers: Partial<KeyboardEvent> = {}) =>
      new KeyboardEvent("keydown", { key, ...modifiers });
    const row = (id: string) => COMPOSER_KEYS.find((entry) => entry.id === id);

    expect(row("send")?.keycaps(true)).toEqual(["↩"]);
    expect(shouldSubmitComposerKey(press("Enter"))).toBe(true);

    expect(row("new-line")?.keycaps(true)).toEqual(["⇧", "↩"]);
    expect(shouldSubmitComposerKey(press("Enter", { shiftKey: true }))).toBe(
      false,
    );

    expect(row("steer")?.keycaps(true)).toEqual(["⌘", "↩"]);
    expect(row("steer")?.keycaps(false)).toEqual(["Ctrl", "↩"]);
    expect(shouldSteerComposerKey(press("Enter", { metaKey: true }))).toBe(
      true,
    );
    expect(shouldSteerComposerKey(press("Enter", { ctrlKey: true }))).toBe(
      true,
    );

    expect(row("stop")?.keycaps(true)).toEqual(["Esc"]);
    expect(shouldStopTurnKey(press("Escape"), document)).toBe(true);
  });
});
