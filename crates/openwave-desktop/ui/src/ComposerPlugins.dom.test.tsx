// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { Composer, type ComposerSlash } from "./Composer";
import type { SlashOption } from "./ComposerSlash";
import type { PluginInfo } from "./api";

afterEach(cleanup);

const DOCUMENTS: PluginInfo = {
  name: "documents",
  display_name: "Documents",
  description: "Writes Word, Excel, and PowerPoint files.",
  category: "documents",
  origin: "builtin",
  capabilities: [],
  enabled: false,
  skills: [],
};

const OPTIONS: SlashOption[] = [
  {
    kind: "plugin",
    name: "documents",
    label: "Documents",
    description: "Writes Word, Excel, and PowerPoint files.",
    category: "documents",
    skills: ["docx", "pptx"],
  },
  {
    kind: "skill",
    name: "docx",
    label: "Docx",
    description: "Writes documents.",
    category: "documents",
  },
  {
    kind: "prompt",
    name: "weekly-update",
    label: "Weekly update",
    description: "The Monday note.",
  },
];

function ComposerHarness({
  slash,
  onDraftChange,
  onSelect = vi.fn(),
}: {
  slash: ComposerSlash;
  onDraftChange?: (draft: string) => void;
  onSelect?: (plugin: PluginInfo) => void;
}) {
  const [draft, setDraft] = useState("Summarize the report");
  return (
    <Composer
      activeTurnId={null}
      busy={false}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft={draft}
      plugins={{ items: [DOCUMENTS], onSelect }}
      slash={slash}
      onDraftChange={(next) => {
        setDraft(next);
        onDraftChange?.(next);
      }}
      onSend={vi.fn(async () => {})}
      onSteer={vi.fn(async () => {})}
      onStop={vi.fn(async () => {})}
      resetKey="chat-1"
      steerError={null}
      steerPending={false}
      steerStatus={null}
    />
  );
}

function slash(overrides: Partial<ComposerSlash> = {}): ComposerSlash {
  return {
    options: OPTIONS,
    invoked: [],
    onInvoke: vi.fn(),
    onRemove: vi.fn(),
    loadPromptBody: vi.fn(async () => "Write this week's update covering:"),
    ...overrides,
  };
}

async function openPanel() {
  const user = userEvent.setup();
  await user.click(screen.getByRole("button", { name: "Tools" }));
  await user.click(screen.getByRole("menuitem", { name: "Plugins" }));
  return user;
}

it("opens the library from the tools menu with everything a slash reaches", async () => {
  render(<ComposerHarness slash={slash()} />);

  await openPanel();
  const rows = await screen.findAllByRole("option");
  expect(rows.map((row) => row.textContent)).toEqual([
    "Documents" + DOCUMENTS.description + "Plugin",
    "DocxWrites documents.Skill",
    "Weekly updateThe Monday note.Prompt",
  ]);
});

it("engages a picked bundle and invokes its skills without touching the draft", async () => {
  const onSelect = vi.fn();
  const onInvoke = vi.fn();
  const onDraftChange = vi.fn();
  render(
    <ComposerHarness
      slash={slash({ onInvoke })}
      onDraftChange={onDraftChange}
      onSelect={onSelect}
    />,
  );

  const user = await openPanel();
  await user.click(await screen.findByRole("option", { name: /Documents/ }));

  // The bundle is turned on, and stands for its members on the message.
  expect(onSelect).toHaveBeenCalledWith(DOCUMENTS);
  expect(onInvoke).toHaveBeenCalledWith(["docx", "pptx"]);
  // Never a sentence in the message the reader is writing.
  expect(onDraftChange).not.toHaveBeenCalled();
});

it("narrows the panel from its own field and picks with the keyboard", async () => {
  const onInvoke = vi.fn();
  render(<ComposerHarness slash={slash({ onInvoke })} />);

  const user = await openPanel();
  const search = await screen.findByRole("textbox", {
    name: "Search the plugin library",
  });
  await user.type(search, "docx");
  expect(screen.getAllByRole("option")).toHaveLength(1);

  await user.keyboard("{Enter}");
  expect(onInvoke).toHaveBeenCalledWith(["docx"]);
  expect(screen.queryByRole("option")).toBeNull();
});

it("drops what this message already carries from what the panel offers", async () => {
  render(<ComposerHarness slash={slash({ invoked: ["docx"] })} />);

  await openPanel();
  const rows = await screen.findAllByRole("option");
  // The bundle stays: it still has a member left to invoke. The invoked skill
  // is gone, and its chip says what the message carries instead.
  expect(rows.map((row) => row.textContent)).toEqual([
    "Documents" + DOCUMENTS.description + "Plugin",
    "Weekly updateThe Monday note.Prompt",
  ]);
  expect(
    screen.getByRole("button", { name: "Remove Docx" }),
  ).toBeInTheDocument();
});
