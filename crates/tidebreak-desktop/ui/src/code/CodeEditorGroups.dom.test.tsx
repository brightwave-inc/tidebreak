// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { CodeEditorGroups } from "./CodeEditorGroups";

afterEach(cleanup);

it("keeps the focused input and its selection when an agent preview opens and closes", () => {
  const primary = (
    <textarea
      aria-label="Source editor"
      defaultValue="const name = 'Tidebreak';"
    />
  );
  const { rerender } = render(<CodeEditorGroups primary={primary} />);
  const editor = screen.getByRole("textbox") as HTMLTextAreaElement;
  editor.focus();
  editor.setSelectionRange(6, 10);
  rerender(
    <CodeEditorGroups primary={primary} secondary={<div>Agent browser</div>} />,
  );
  expect(screen.getByRole("textbox")).toBe(editor);
  expect(document.activeElement).toBe(editor);
  expect([editor.selectionStart, editor.selectionEnd]).toEqual([6, 10]);
  rerender(<CodeEditorGroups primary={primary} />);
  expect(screen.getByRole("textbox")).toBe(editor);
  expect(document.activeElement).toBe(editor);
  expect([editor.selectionStart, editor.selectionEnd]).toEqual([6, 10]);
});
