// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";

import { Composer, type ComposerFiles, type ComposerFolders } from "./Composer";

afterEach(cleanup);

const RECENT = [
  { documentId: "doc-1", name: "Budget.xlsx", mediaType: "text/csv" },
];

function ComposerHarness({
  files,
  folders,
  onDraftChange,
}: {
  files: ComposerFiles;
  folders?: ComposerFolders;
  onDraftChange?: (draft: string) => void;
}) {
  const [draft, setDraft] = useState("");
  return (
    <Composer
      activeTurnId={null}
      busy={false}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft={draft}
      files={files}
      folders={folders}
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

function composerFiles(overrides: Partial<ComposerFiles> = {}): ComposerFiles {
  return {
    items: [],
    recent: RECENT,
    attaching: false,
    onAttach: vi.fn(),
    onReattach: vi.fn(),
    onRemove: vi.fn(),
    ...overrides,
  };
}

function composerFolders(
  overrides: Partial<ComposerFolders> = {},
): ComposerFolders {
  return {
    items: [],
    approved: [
      {
        rootId: "root-1",
        displayName: "Reports",
        status: "connected",
        availableInFutureChats: true,
      },
    ],
    working: false,
    error: null,
    onAttach: vi.fn(),
    onConnect: vi.fn(),
    onRemove: vi.fn(),
    ...overrides,
  };
}

it("attaches a file by name and takes the token back out of the draft", async () => {
  const user = userEvent.setup();
  const onReattach = vi.fn();
  const onDraftChange = vi.fn();
  render(
    <ComposerHarness
      files={composerFiles({ onReattach })}
      folders={composerFolders()}
      onDraftChange={onDraftChange}
    />,
  );

  await user.click(screen.getByRole("textbox", { name: "Message" }));
  await user.keyboard("compare against @budg");
  await user.click(screen.getByRole("option", { name: /Budget.xlsx/ }));

  expect(onReattach).toHaveBeenCalledWith(RECENT[0]);
  expect(onDraftChange).toHaveBeenLastCalledWith("compare against ");
});

it("attaches an approved folder from the keyboard alone", async () => {
  const user = userEvent.setup();
  const onConnect = vi.fn();
  render(
    <ComposerHarness
      files={composerFiles()}
      folders={composerFolders({ onConnect })}
    />,
  );

  await user.click(screen.getByRole("textbox", { name: "Message" }));
  await user.keyboard("@repor{Enter}");

  expect(onConnect).toHaveBeenCalledWith("root-1");
});

it("leaves an at-sign inside ordinary text alone, and closes on escape", async () => {
  const user = userEvent.setup();
  render(
    <ComposerHarness files={composerFiles()} folders={composerFolders()} />,
  );

  const field = screen.getByRole("textbox", { name: "Message" });
  await user.click(field);
  // Mid-word: an email address is not someone reaching for an attachment.
  await user.keyboard("write to ada@example.com");
  expect(screen.queryByRole("option")).toBeNull();

  await user.clear(field);
  await user.keyboard("@");
  expect(screen.getAllByRole("option").length).toBeGreaterThan(0);
  await user.keyboard("{Escape}");
  expect(screen.queryByRole("option")).toBeNull();
});

it("falls back to the pickers when nothing is within reach", async () => {
  const user = userEvent.setup();
  const onAttach = vi.fn();
  render(<ComposerHarness files={composerFiles({ recent: [], onAttach })} />);

  await user.click(screen.getByRole("textbox", { name: "Message" }));
  await user.keyboard("@");
  await user.click(screen.getByRole("option", { name: /Browse files/ }));

  expect(onAttach).toHaveBeenCalled();
});
