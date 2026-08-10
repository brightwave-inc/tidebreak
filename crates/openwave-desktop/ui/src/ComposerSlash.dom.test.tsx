// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { Composer, type ComposerSlash } from "./Composer";
import { SLASH_COMMANDS } from "./ComposerCommands";
import type { SlashOption } from "./ComposerSlash";

afterEach(cleanup);

const OPTIONS: SlashOption[] = [
  {
    kind: "skill",
    name: "pptx",
    label: "Pptx",
    description: "Builds slide decks.",
  },
  {
    kind: "skill",
    name: "xlsx",
    label: "Xlsx",
    description: "Builds spreadsheets.",
  },
  {
    kind: "prompt",
    name: "weekly-update",
    label: "Weekly update",
    description: "The Monday note.",
  },
];

function slash(overrides: Partial<ComposerSlash> = {}): ComposerSlash {
  return {
    options: OPTIONS,
    invoked: [],
    onInvoke: vi.fn(),
    onRemove: vi.fn(),
    loadPromptBody: vi.fn(async () => "Write this week's update covering:"),
    onCommand: vi.fn(),
    ...overrides,
  };
}

/**
 * A composer whose draft is real state, so typing behaves the way it does in a
 * route: the `/` token the list reads is whatever has actually been typed.
 */
function ComposerHarness({
  slash: menu,
  onDraftChange,
  onSend = vi.fn(async () => {}),
  activeTurnId = null,
}: {
  slash: ComposerSlash;
  onDraftChange?: (draft: string) => void;
  onSend?: () => Promise<void>;
  activeTurnId?: string | null;
}) {
  const [draft, setDraft] = useState("");
  return (
    <Composer
      activeTurnId={activeTurnId}
      busy={activeTurnId !== null}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft={draft}
      slash={menu}
      onDraftChange={(next) => {
        setDraft(next);
        onDraftChange?.(next);
      }}
      onSend={onSend}
      onSteer={vi.fn(async () => {})}
      onStop={vi.fn(async () => {})}
      resetKey="chat-1"
      steerError={null}
      steerPending={false}
      steerStatus={null}
    />
  );
}

it("opens on a leading slash, narrows to what is typed, and closes on escape", async () => {
  const user = userEvent.setup();
  render(<ComposerHarness slash={slash()} />);

  await user.click(screen.getByRole("textbox", { name: "Message" }));
  await user.keyboard("/");
  expect(screen.getAllByRole("option")).toHaveLength(
    OPTIONS.length + SLASH_COMMANDS.length,
  );

  await user.keyboard("ppt");
  const narrowed = screen.getAllByRole("option");
  expect(narrowed).toHaveLength(1);
  expect(narrowed[0]).toHaveAccessibleName(/Pptx/);

  await user.keyboard("{Escape}");
  expect(screen.queryByRole("option")).toBeNull();
});

it("leaves a slash inside ordinary text alone", async () => {
  const user = userEvent.setup();
  render(<ComposerHarness slash={slash()} />);

  const field = screen.getByRole("textbox", { name: "Message" });
  await user.click(field);
  // Mid-word: a path is not someone reaching for the library.
  await user.keyboard("read src/pptx");
  expect(screen.queryByRole("option")).toBeNull();

  // And a space ends the token: by then the reader is writing prose again.
  await user.clear(field);
  await user.keyboard("/pptx and");
  expect(screen.queryByRole("option")).toBeNull();
});

it("replaces the token with the prompt body it fetches", async () => {
  const user = userEvent.setup();
  const menu = slash();
  const onDraftChange = vi.fn();
  render(<ComposerHarness slash={menu} onDraftChange={onDraftChange} />);

  await user.click(screen.getByRole("textbox", { name: "Message" }));
  await user.keyboard("draft the /weekly");
  await user.click(screen.getByRole("option", { name: /Weekly update/ }));

  expect(menu.loadPromptBody).toHaveBeenCalledWith("weekly-update");
  expect(onDraftChange).toHaveBeenLastCalledWith(
    "draft the Write this week's update covering:",
  );
  // A prompt is text, not an invocation: nothing is pinned to the message.
  expect(menu.onInvoke).not.toHaveBeenCalled();
});

it("invokes a picked skill and shows it as a chip the reader can drop", async () => {
  const user = userEvent.setup();
  const onInvoke = vi.fn();
  const onDraftChange = vi.fn();
  render(
    <ComposerHarness slash={slash({ onInvoke })} onDraftChange={onDraftChange} />,
  );

  await user.click(screen.getByRole("textbox", { name: "Message" }));
  await user.keyboard("make slides /pptx");
  await user.click(screen.getByRole("option", { name: /Pptx/ }));

  expect(onInvoke).toHaveBeenCalledWith(["pptx"]);
  // The token comes out of the prose; the invocation travels beside it.
  expect(onDraftChange).toHaveBeenLastCalledWith("make slides ");

  cleanup();
  const onRemove = vi.fn();
  render(<ComposerHarness slash={slash({ invoked: ["pptx"], onRemove })} />);
  await user.click(screen.getByRole("button", { name: "Remove Pptx" }));
  expect(onRemove).toHaveBeenCalledWith("pptx");
});

it("runs a built-in command instead of sending it as a message", async () => {
  const user = userEvent.setup();
  const onCommand = vi.fn();
  const onSend = vi.fn(async () => {});
  render(<ComposerHarness slash={slash({ onCommand })} onSend={onSend} />);

  await user.click(screen.getByRole("textbox", { name: "Message" }));
  await user.keyboard("/usage{Enter}");

  expect(onCommand).toHaveBeenCalledWith("usage", "");
  expect(onSend).not.toHaveBeenCalled();
});

it("runs a command typed out with the list dismissed", async () => {
  // The path a command with an argument takes: the token's list closes at the
  // first space, so Enter has to recognise the line rather than the highlight.
  const user = userEvent.setup();
  const onCommand = vi.fn();
  const onSend = vi.fn(async () => {});
  render(<ComposerHarness slash={slash({ onCommand })} onSend={onSend} />);

  await user.click(screen.getByRole("textbox", { name: "Message" }));
  await user.keyboard("/usage{Escape}{Enter}");

  expect(onCommand).toHaveBeenCalledWith("usage", "");
  expect(onSend).not.toHaveBeenCalled();
});

it("shows a bundle a running turn cannot reach, and refuses the pick", async () => {
  const user = userEvent.setup();
  const onInvoke = vi.fn();
  const menu = slash({
    onInvoke,
    options: [
      ...OPTIONS,
      {
        kind: "plugin",
        name: "charts",
        label: "Charts",
        description: "Draws figures.",
        skills: ["charts"],
        enabled: false,
      },
    ],
  });
  render(<ComposerHarness slash={menu} activeTurnId="turn-1" />);

  await user.click(screen.getByRole("textbox", { name: "Message" }));
  await user.keyboard("also /charts");
  // Visible, so a library seen a moment ago has not silently lost an entry —
  // and marked, because turning it on mid-turn names a manifest this turn's
  // workspace never staged.
  const row = screen.getByRole("option", { name: /Charts/ });
  expect(row).toHaveAttribute("aria-disabled", "true");
  await user.click(row);
  expect(onInvoke).not.toHaveBeenCalled();
});
