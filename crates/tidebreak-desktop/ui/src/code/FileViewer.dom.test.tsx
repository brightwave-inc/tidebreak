// @vitest-environment jsdom
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { HttpError } from "@/api/client/http";
import type { CodeWorkspaceBlob } from "@/api/types";
import { clearFileDownloadCache } from "@/document/useFileDownload";
import { useCodeFileDraftStore } from "./CodeFileDraftStore";
import { useCodeUpdatesStore } from "./CodeUpdatesStore";
import { FileViewer, imageMediaTypeForPath } from "./FileViewer";

/**
 * Monaco does not run in jsdom. A textarea stands in for it: the same value,
 * read-only option, and change callback the viewer drives.
 */
vi.mock("@monaco-editor/react", () => ({
  default: ({
    value,
    options,
    onChange,
  }: {
    value?: string;
    options?: { readOnly?: boolean };
    onChange?: (value: string) => void;
  }) => (
    <textarea
      aria-label="Text editor"
      value={value ?? ""}
      readOnly={options?.readOnly ?? false}
      onChange={(event) => onChange?.(event.target.value)}
    />
  ),
  DiffEditor: ({
    original,
    modified,
  }: {
    original?: string;
    modified?: string;
  }) => (
    <div data-testid="diff-editor">
      <pre data-testid="diff-original">{original}</pre>
      <pre data-testid="diff-modified">{modified}</pre>
    </div>
  ),
  loader: { config: vi.fn() },
}));

const LOADED_HASH = "a".repeat(64);
const SAVED_HASH = "b".repeat(64);
const DISK_HASH = "c".repeat(64);

function textBlob(
  content: string,
  overrides: Partial<CodeWorkspaceBlob> = {},
): CodeWorkspaceBlob {
  return {
    path: "src/main.rs",
    content,
    truncated: false,
    binary: false,
    hash: LOADED_HASH,
    ...overrides,
  };
}

function textClient(blob: CodeWorkspaceBlob = textBlob("fn main() {}\n")) {
  return {
    getCodeWorkspaceBlob: vi.fn().mockResolvedValue(blob),
    getCodeWorkspaceFile: vi.fn(),
    saveCodeWorkspaceFile: vi
      .fn()
      .mockResolvedValue({ path: blob.path, hash: SAVED_HASH }),
  };
}

function fileChanged(currentHash: string): HttpError {
  return new HttpError(
    409,
    "409: src/main.rs changed on disk since you opened it.",
    "file_changed",
    {
      kind: "file_changed",
      message: "src/main.rs changed on disk since you opened it.",
      current_hash: currentHash,
    },
  );
}

async function openEditor(client: ReturnType<typeof textClient>) {
  render(
    <FileViewer client={client} workspaceId="workspace-1" path="src/main.rs" />,
  );
  fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
  return screen.getByRole("textbox", { name: "Text editor" });
}

function type(editor: HTMLElement, text: string) {
  fireEvent.change(editor, { target: { value: text } });
}

beforeEach(() => {
  clearFileDownloadCache();
  URL.createObjectURL = vi.fn(() => "blob:workspace-image");
  URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  cleanup();
  useCodeFileDraftStore.setState({ drafts: {} });
  useCodeUpdatesStore.getState().reset();
});

describe("FileViewer", () => {
  it("opens a workspace image from its original bytes", async () => {
    const client = {
      getCodeWorkspaceBlob: vi.fn().mockResolvedValue({
        path: "screenshots/review.webp",
        content: "",
        truncated: false,
        binary: true,
      }),
      getCodeWorkspaceFile: vi.fn().mockResolvedValue({
        bytes: new Uint8Array([1, 2, 3]),
        contentType: "image/webp",
      }),
      saveCodeWorkspaceFile: vi.fn(),
    };

    render(
      <FileViewer
        client={client}
        workspaceId="workspace-1"
        path="screenshots/review.webp"
      />,
    );

    expect(
      await screen.findByRole("img", { name: "Document image" }),
    ).toHaveAttribute("src", "blob:workspace-image");
    expect(client.getCodeWorkspaceFile).toHaveBeenCalledWith(
      "workspace-1",
      "screenshots/review.webp",
      expect.any(AbortSignal),
      expect.any(Function),
    );
    expect(screen.getByText("Images are read-only")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Edit" })).toBeNull();
  });

  it("reloads image bytes when the workspace content revision changes", async () => {
    const client = {
      getCodeWorkspaceBlob: vi.fn().mockResolvedValue({
        path: "screenshots/review.webp",
        content: "",
        truncated: false,
        binary: true,
      }),
      getCodeWorkspaceFile: vi.fn().mockResolvedValue({
        bytes: new Uint8Array([1, 2, 3]),
        contentType: "image/webp",
      }),
      saveCodeWorkspaceFile: vi.fn(),
    };

    const { rerender } = render(
      <FileViewer
        client={client}
        workspaceId="workspace-1"
        path="screenshots/review.webp"
        contentRevision={1}
      />,
    );
    await screen.findByRole("img", { name: "Document image" });

    rerender(
      <FileViewer
        client={client}
        workspaceId="workspace-1"
        path="screenshots/review.webp"
        contentRevision={2}
      />,
    );
    await vi.waitFor(() => {
      expect(client.getCodeWorkspaceFile).toHaveBeenCalledTimes(2);
    });
  });

  it("keeps an unsupported binary file on the fallback", async () => {
    const client = {
      getCodeWorkspaceBlob: vi.fn().mockResolvedValue({
        path: "archive.zip",
        content: "",
        truncated: false,
        binary: true,
      }),
      getCodeWorkspaceFile: vi.fn(),
      saveCodeWorkspaceFile: vi.fn(),
    };

    render(
      <FileViewer
        client={client}
        workspaceId="workspace-1"
        path="archive.zip"
      />,
    );

    expect(
      await screen.findByText(
        "This file is binary, so the editor does not open it.",
      ),
    ).toBeVisible();
    expect(client.getCodeWorkspaceFile).not.toHaveBeenCalled();
    expect(screen.getByText("Binary files are read-only")).toBeVisible();
  });

  it("makes the editor writable when you choose Edit", async () => {
    const client = textClient();
    render(
      <FileViewer
        client={client}
        workspaceId="workspace-1"
        path="src/main.rs"
      />,
    );
    const editor = await screen.findByRole("textbox", { name: "Text editor" });
    await waitFor(() => expect(editor).toHaveValue("fn main() {}\n"));
    expect(editor).toHaveAttribute("readonly");

    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    expect(
      screen.getByRole("textbox", { name: "Text editor" }),
    ).not.toHaveAttribute("readonly");
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  });

  it("saves with Cmd+S against the hash the file was loaded at", async () => {
    const client = textClient();
    const editor = await openEditor(client);

    type(editor, 'fn main() { println!("hi"); }\n');
    expect(screen.getByText("Unsaved changes")).toBeVisible();
    fireEvent.keyDown(editor, { key: "s", code: "KeyS", metaKey: true });

    await waitFor(() =>
      expect(client.saveCodeWorkspaceFile).toHaveBeenCalledWith("workspace-1", {
        path: "src/main.rs",
        content: 'fn main() { println!("hi"); }\n',
        base_hash: LOADED_HASH,
      }),
    );
    expect(await screen.findByText("Saved")).toBeVisible();
    expect(useCodeUpdatesStore.getState().fileRevisions["workspace-1"]).toBe(1);

    // The next save names the hash the last one returned.
    type(
      screen.getByRole("textbox", { name: "Text editor" }),
      "fn main() {}\n",
    );
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(client.saveCodeWorkspaceFile).toHaveBeenLastCalledWith(
        "workspace-1",
        expect.objectContaining({ base_hash: SAVED_HASH }),
      ),
    );
  });

  it("keeps the text and shows why when a save fails", async () => {
    const client = textClient();
    client.saveCodeWorkspaceFile.mockRejectedValue(
      new HttpError(
        413,
        "413: The new text is over 512 KB, the most the editor saves.",
        "payload_too_large",
      ),
    );
    const editor = await openEditor(client);

    type(editor, "fn main() { big(); }\n");
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(
      "Your changes are not saved. The new text is over 512 KB, the most the editor saves.",
    );
    expect(screen.getByRole("textbox", { name: "Text editor" })).toHaveValue(
      "fn main() { big(); }\n",
    );
  });

  it("offers to compare and overwrite when a save finds the file changed", async () => {
    const client = textClient();
    client.saveCodeWorkspaceFile
      .mockRejectedValueOnce(fileChanged(DISK_HASH))
      .mockResolvedValueOnce({ path: "src/main.rs", hash: SAVED_HASH });
    const editor = await openEditor(client);

    type(editor, "fn main() { mine(); }\n");
    fireEvent.keyDown(editor, { key: "s", code: "KeyS", metaKey: true });

    const notice = await screen.findByRole("alert");
    expect(notice).toHaveTextContent(
      "This file changed on disk. Your changes are not saved.",
    );
    for (const name of ["Reload", "Keep my changes", "Compare", "Overwrite"]) {
      expect(within(notice).getByRole("button", { name })).toBeVisible();
    }
    expect(screen.getByRole("textbox", { name: "Text editor" })).toHaveValue(
      "fn main() { mine(); }\n",
    );

    client.getCodeWorkspaceBlob.mockResolvedValue(
      textBlob("fn main() { agent(); }\n", { hash: DISK_HASH }),
    );
    fireEvent.click(within(notice).getByRole("button", { name: "Compare" }));
    expect(await screen.findByTestId("diff-original")).toHaveTextContent(
      "fn main() { agent(); }",
    );
    expect(screen.getByTestId("diff-modified")).toHaveTextContent(
      "fn main() { mine(); }",
    );

    fireEvent.click(within(notice).getByRole("button", { name: "Overwrite" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(within(dialog).getByText("Overwrite main.rs?")).toBeVisible();
    fireEvent.click(within(dialog).getByRole("button", { name: "Overwrite" }));

    await waitFor(() =>
      expect(client.saveCodeWorkspaceFile).toHaveBeenLastCalledWith(
        "workspace-1",
        {
          path: "src/main.rs",
          content: "fn main() { mine(); }\n",
          base_hash: DISK_HASH,
        },
      ),
    );
    await waitFor(() =>
      expect(screen.queryByText("This file changed on disk.")).toBeNull(),
    );
    expect(screen.getByText("Saved")).toBeVisible();
  });

  it("keeps your buffer when the file changes on disk under it", async () => {
    const client = textClient();
    const { rerender } = render(
      <FileViewer
        client={client}
        workspaceId="workspace-1"
        path="src/main.rs"
        contentRevision={1}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
    type(screen.getByRole("textbox", { name: "Text editor" }), "mine\n");

    client.getCodeWorkspaceBlob.mockResolvedValue(
      textBlob("the agent's\n", { hash: DISK_HASH }),
    );
    rerender(
      <FileViewer
        client={client}
        workspaceId="workspace-1"
        path="src/main.rs"
        contentRevision={2}
      />,
    );

    const notice = await screen.findByRole("alert");
    expect(notice).toHaveTextContent("This file changed on disk.");
    expect(
      within(notice).queryByRole("button", { name: "Overwrite" }),
    ).toBeNull();
    expect(screen.getByRole("textbox", { name: "Text editor" })).toHaveValue(
      "mine\n",
    );

    fireEvent.click(within(notice).getByRole("button", { name: "Reload" }));
    await waitFor(() =>
      expect(screen.getByRole("textbox", { name: "Text editor" })).toHaveValue(
        "the agent's\n",
      ),
    );
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("restores the loaded text when you discard your changes", async () => {
    const client = textClient();
    const editor = await openEditor(client);

    type(editor, "something else entirely\n");
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    const dialog = await screen.findByRole("alertdialog");
    expect(
      within(dialog).getByText("Discard unsaved changes to main.rs?"),
    ).toBeVisible();
    fireEvent.click(within(dialog).getByRole("button", { name: "Discard" }));

    await waitFor(() =>
      expect(
        screen.getByRole("textbox", { name: "Text editor" }),
      ).toHaveAttribute("readonly"),
    );
    expect(screen.getByRole("textbox", { name: "Text editor" })).toHaveValue(
      "fn main() {}\n",
    );
    expect(client.saveCodeWorkspaceFile).not.toHaveBeenCalled();
    expect(useCodeFileDraftStore.getState().drafts).toEqual({});
  });

  it("keeps unsaved text across a remount, as when you switch tabs", async () => {
    const client = textClient();
    const editor = await openEditor(client);
    type(editor, "still mine\n");
    cleanup();

    render(
      <FileViewer
        client={client}
        workspaceId="workspace-1"
        path="src/main.rs"
      />,
    );
    await waitFor(() =>
      expect(screen.getByRole("textbox", { name: "Text editor" })).toHaveValue(
        "still mine\n",
      ),
    );
    expect(screen.getByText("Unsaved changes")).toBeVisible();
  });

  it("keeps each file's buffer its own when the viewer moves to another file", async () => {
    const client = textClient(textBlob("fn a() {}\n", { path: "src/a.rs" }));
    // B is open for editing in another tab, with nothing unsaved yet.
    act(() =>
      useCodeFileDraftStore.getState().startEditing("workspace-1", "src/b.rs", {
        text: "fn b() {}\n",
        hash: DISK_HASH,
      }),
    );
    const { rerender } = render(
      <FileViewer client={client} workspaceId="workspace-1" path="src/a.rs" />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Edit" }));
    type(screen.getByRole("textbox", { name: "Text editor" }), "fn a2() {}\n");

    // B's read is still in flight, so all the viewer has for B is its draft.
    client.getCodeWorkspaceBlob.mockReturnValue(new Promise(() => {}));
    rerender(
      <FileViewer client={client} workspaceId="workspace-1" path="src/b.rs" />,
    );

    await waitFor(() =>
      expect(client.getCodeWorkspaceBlob).toHaveBeenLastCalledWith(
        "workspace-1",
        "src/b.rs",
      ),
    );
    expect(screen.getByRole("textbox", { name: "Text editor" })).toHaveValue(
      "fn b() {}\n",
    );
    const drafts = Object.values(useCodeFileDraftStore.getState().drafts);
    expect(
      drafts.map(({ path, baseText, text, conflict }) => ({
        path,
        baseText,
        text,
        conflict,
      })),
    ).toEqual([
      {
        path: "src/b.rs",
        baseText: "fn b() {}\n",
        text: "fn b() {}\n",
        conflict: null,
      },
      {
        path: "src/a.rs",
        baseText: "fn a() {}\n",
        text: "fn a2() {}\n",
        conflict: null,
      },
    ]);
  });

  it.each([
    [
      "a file the viewer cut short",
      textBlob("fn main() {", { truncated: true, hash: undefined }),
      undefined,
      "Files over 512 KB are read-only",
    ],
    [
      "text that is not UTF-8",
      textBlob("caf\uFFFD\n", { hash: undefined }),
      undefined,
      "Files that are not UTF-8 are read-only",
    ],
    [
      "a sandbox checkpoint",
      textBlob("fn main() {}\n", {
        hash: undefined,
        revision: "retained",
        revision_ref: "mg-wip/sb-1-i1",
      }),
      undefined,
      "Saved checkpoints are read-only",
    ],
    [
      "a sandbox workspace",
      textBlob("fn main() {}\n"),
      "Sandbox files are read-only",
      "Sandbox files are read-only",
    ],
  ])("offers no Edit for %s and says why", async (_, blob, pageReason, why) => {
    render(
      <FileViewer
        client={textClient(blob)}
        workspaceId="workspace-1"
        path="src/main.rs"
        readOnlyReason={pageReason}
      />,
    );

    expect(await screen.findByText(why)).toBeVisible();
    expect(screen.queryByRole("button", { name: "Edit" })).toBeNull();
  });

  it("renders markdown with a preview it can switch back to source", async () => {
    const client = textClient(
      textBlob("# Release notes\n\nEditing works.\n", { path: "README.md" }),
    );
    render(
      <FileViewer client={client} workspaceId="workspace-1" path="README.md" />,
    );

    const preview = await screen.findByRole("document", {
      name: "Markdown preview",
    });
    expect(
      within(preview).getByRole("heading", { name: "Release notes" }),
    ).toBeVisible();
    const toggle = screen.getByRole("button", { name: "Preview" });
    expect(toggle).toHaveAttribute("aria-pressed", "true");

    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    expect(toggle).toHaveAttribute("aria-pressed", "false");
    type(
      screen.getByRole("textbox", { name: "Text editor" }),
      "# Release notes\n\nEditing works well.\n",
    );
    act(() => fireEvent.click(toggle));
    expect(await screen.findByText("Editing works well.")).toBeInTheDocument();
  });
});

it("recognizes browser image extensions without case sensitivity", () => {
  expect(imageMediaTypeForPath("assets/PHOTO.JPEG")).toBe("image/jpeg");
  expect(imageMediaTypeForPath("assets/diagram.svg")).toBe("image/svg+xml");
  expect(imageMediaTypeForPath("assets/archive.zip")).toBeNull();
});
