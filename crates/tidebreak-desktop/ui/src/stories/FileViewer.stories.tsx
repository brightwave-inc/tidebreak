import { useState } from "react";
import type { Meta, StoryObj } from "@storybook/react-vite";
import { expect, userEvent, waitFor, within } from "storybook/test";

import type { ApiClient } from "@/api/client";
import { HttpError } from "@/api/client/http";
import type { CodeWorkspaceBlob } from "@/api/types";
import {
  codeFileDraftKey,
  useCodeFileDraftStore,
  type CodeFileDraft,
} from "@/code/CodeFileDraftStore";
import { FileViewer } from "@/code/FileViewer";

type FileScenario =
  | "image"
  | "image-failure"
  | "unsupported"
  | "retained"
  | "live"
  | "loading"
  | "unavailable"
  | "read-only"
  | "editing"
  | "saving"
  | "saved"
  | "save-failed"
  | "conflict"
  | "changed-on-disk"
  | "markdown-preview"
  | "markdown-editing"
  | "edit-unavailable";

const WORKSPACE = "workspace-storybook";

/** Long enough that a narrow header has to choose between path and chip. */
const LIVE_PATH =
  "crates/tidebreak-server/src/code/remote/checkpoints/periodic_push.rs";

const SOURCE_PATH = "crates/tidebreak-server/src/code/file_save.rs";

const LOADED_HASH = "3f".repeat(32);
const SAVED_HASH = "7c".repeat(32);
const DISK_HASH = "a1".repeat(32);

const SOURCE = `/// The most one save may write: the viewer's own cap, so every file the
/// editor writes is one the viewer shows whole.
pub const MAX_SAVE_BYTES: usize = worktree::MAX_BLOB_BYTES;

/// Whether \`value\` has the shape [\`worktree::content_hash\`] produces.
pub fn is_content_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
`;

const EDITED_SOURCE = SOURCE.replace(
  "pub fn is_content_hash",
  "#[must_use]\npub fn is_content_hash",
);

const AGENT_SOURCE = SOURCE.replace(
  "/// Whether `value`",
  "/// Whether `value`, as the blob route writes it,",
);

const README = `# Tidebreak

Tidebreak runs coding agents in isolated workspaces and keeps you in the loop.

## Editing files

- Open a file from the **Files** tab.
- Choose **Edit**, change the text, and press \`⌘S\` to save.
- When an agent changes the same file, you choose which version to keep.

| Surface | What it shows |
| --- | --- |
| Files | The worktree, one file at a time |
| Changes | What differs from the base branch |
`;

const EDITED_README = README.replace(
  "## Editing files",
  "## Editing files\n\nMarkdown files also get a preview.",
);

const imageSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="800" viewBox="0 0 1200 800">
  <rect width="1200" height="800" fill="#e8ecef"/>
  <rect x="72" y="72" width="1056" height="656" rx="24" fill="#fbfcfd" stroke="#c7d0d8"/>
  <text x="124" y="156" font-family="system-ui, sans-serif" font-size="30" font-weight="600" fill="#23313c">Workspace architecture</text>
  <text x="124" y="196" font-family="system-ui, sans-serif" font-size="17" fill="#687985">Review capture · September 9, 2026</text>
  <rect x="124" y="258" width="260" height="166" rx="14" fill="#dce8e7" stroke="#aac2c0"/>
  <rect x="470" y="258" width="260" height="166" rx="14" fill="#e5e8f0" stroke="#b8bfd0"/>
  <rect x="816" y="258" width="260" height="166" rx="14" fill="#e9e5ef" stroke="#c6bcd0"/>
  <path d="M384 341h86M730 341h86" stroke="#7c8c97" stroke-width="4" stroke-linecap="round"/>
  <text x="160" y="334" font-family="system-ui, sans-serif" font-size="22" font-weight="600" fill="#29413f">Conversation</text>
  <text x="506" y="334" font-family="system-ui, sans-serif" font-size="22" font-weight="600" fill="#30394d">Workspace</text>
  <text x="852" y="334" font-family="system-ui, sans-serif" font-size="22" font-weight="600" fill="#42364e">Review</text>
  <text x="160" y="374" font-family="system-ui, sans-serif" font-size="16" fill="#61706f">Intent and progress</text>
  <text x="506" y="374" font-family="system-ui, sans-serif" font-size="16" fill="#697184">Files and terminal</text>
  <text x="852" y="374" font-family="system-ui, sans-serif" font-size="16" fill="#766b7e">Checks and comments</text>
  <rect x="124" y="500" width="952" height="142" rx="14" fill="#f2f4f5" stroke="#d4dadd"/>
  <text x="160" y="555" font-family="ui-monospace, monospace" font-size="16" fill="#52616b">assets/workspace-architecture.svg</text>
  <text x="160" y="598" font-family="system-ui, sans-serif" font-size="18" fill="#33424c">Image files open in the same center pane as source files.</text>
</svg>`;

function pathFor(scenario: FileScenario): string {
  switch (scenario) {
    case "unsupported":
      return "build/archive.zip";
    case "live":
      return LIVE_PATH;
    case "retained":
    case "unavailable":
    case "loading":
      return "sandbox.txt";
    case "image":
    case "image-failure":
      return "assets/workspace-architecture.svg";
    case "markdown-preview":
    case "markdown-editing":
      return "README.md";
    case "edit-unavailable":
      return "fixtures/recorded-session.ndjson";
    default:
      return SOURCE_PATH;
  }
}

function textBlob(
  path: string,
  content: string,
  hash: string | undefined,
): CodeWorkspaceBlob {
  return {
    path,
    content,
    truncated: false,
    binary: false,
    ...(hash ? { hash } : {}),
  };
}

function blobFor(scenario: FileScenario): Promise<CodeWorkspaceBlob> {
  const path = pathFor(scenario);
  switch (scenario) {
    case "loading":
      return new Promise<never>(() => {});
    case "unavailable":
      return Promise.reject(
        new Error(
          "The sandbox has not saved a checkpoint yet. Its first live checkpoint lands within about a minute of the turn starting. Open the transcript meanwhile.",
        ),
      );
    case "live":
      return Promise.resolve({
        path,
        content:
          "/// Pushed about once a minute while a turn runs.\npub const PERIODIC_SECS: u64 = 60;\n",
        truncated: false,
        binary: false,
        revision: "live" as const,
        revision_ref: "mg-wip/sb-1-i2",
        revision_saved_at: new Date(Date.now() - 42_000).toISOString(),
      });
    case "retained":
      return Promise.resolve({
        path,
        content: "from the checkpoint\n",
        truncated: false,
        binary: false,
        revision: "retained" as const,
        revision_ref: "mg-wip/sb-1-i1",
        revision_saved_at: "2026-09-21T18:23:00.000Z",
      });
    case "image":
    case "image-failure":
    case "unsupported":
      return Promise.resolve({
        path,
        content: "",
        truncated: false,
        binary: true,
      });
    case "markdown-preview":
    case "markdown-editing":
      return Promise.resolve(textBlob(path, README, LOADED_HASH));
    case "edit-unavailable":
      return Promise.resolve({
        ...textBlob(
          path,
          '{"type":"session_started","harness_kind":"claude_code"}\n'.repeat(
            40,
          ),
          undefined,
        ),
        truncated: true,
      });
    case "conflict":
    case "changed-on-disk":
      return Promise.resolve(textBlob(path, AGENT_SOURCE, DISK_HASH));
    case "saved":
      return Promise.resolve(textBlob(path, EDITED_SOURCE, SAVED_HASH));
    default:
      return Promise.resolve(textBlob(path, SOURCE, LOADED_HASH));
  }
}

function clientFor(
  scenario: FileScenario,
): Pick<
  ApiClient,
  "getCodeWorkspaceBlob" | "getCodeWorkspaceFile" | "saveCodeWorkspaceFile"
> {
  return {
    getCodeWorkspaceBlob: () => blobFor(scenario),
    getCodeWorkspaceFile: async () => {
      if (scenario === "image-failure") {
        throw new Error("The workspace file could not be read.");
      }
      return {
        bytes: new TextEncoder().encode(imageSvg),
        contentType: "image/svg+xml",
      };
    },
    saveCodeWorkspaceFile: async (_workspaceId, body) => {
      if (scenario === "conflict" && body.base_hash !== DISK_HASH) {
        throw new HttpError(
          409,
          `409: ${body.path} changed on disk since you opened it.`,
          "file_changed",
          { current_hash: DISK_HASH },
        );
      }
      return { path: body.path, hash: SAVED_HASH };
    },
  };
}

/** The draft each editing scenario starts from, as if you had typed it. */
function draftFor(scenario: FileScenario): CodeFileDraft | null {
  const path = pathFor(scenario);
  const base = {
    workspaceId: WORKSPACE,
    path,
    baseText: SOURCE,
    baseHash: LOADED_HASH,
    text: EDITED_SOURCE,
    save: { kind: "idle" as const },
    conflict: null,
    keptOverHash: null,
  };
  switch (scenario) {
    case "editing":
    case "changed-on-disk":
      return base;
    case "saving":
      return { ...base, save: { kind: "saving" } };
    case "saved":
      return {
        ...base,
        baseText: EDITED_SOURCE,
        baseHash: SAVED_HASH,
        save: { kind: "saved" },
      };
    case "save-failed":
      return {
        ...base,
        save: {
          kind: "failed",
          message:
            "You do not have permission to write crates/tidebreak-server/src/code/file_save.rs.",
        },
      };
    case "conflict":
      return {
        ...base,
        conflict: { diskHash: DISK_HASH, rejected: true },
      };
    case "markdown-editing":
      return {
        ...base,
        baseText: README,
        text: EDITED_README,
      };
    default:
      return null;
  }
}

function FileViewerStory({
  scenario,
  pageReason,
}: {
  scenario: FileScenario;
  pageReason?: string;
}) {
  // Seed the draft store before the viewer mounts, so each story opens in
  // its state rather than animating into it.
  const [client] = useState(() => {
    const draft = draftFor(scenario);
    useCodeFileDraftStore.setState({
      drafts: draft ? { [codeFileDraftKey(WORKSPACE, draft.path)]: draft } : {},
    });
    return clientFor(scenario);
  });
  return (
    <div className="flex h-[680px] min-h-0 flex-col overflow-hidden rounded-lg border bg-page-background">
      <FileViewer
        client={client}
        workspaceId={WORKSPACE}
        path={pathFor(scenario)}
        readOnlyReason={pageReason}
      />
    </div>
  );
}

const meta = {
  title: "Code/File viewer",
  component: FileViewerStory,
  args: { scenario: "image" },
  argTypes: {
    scenario: {
      control: "select",
      options: [
        "image",
        "image-failure",
        "unsupported",
        "retained",
        "live",
        "loading",
        "unavailable",
        "read-only",
        "editing",
        "saving",
        "saved",
        "save-failed",
        "conflict",
        "changed-on-disk",
        "markdown-preview",
        "markdown-editing",
        "edit-unavailable",
      ],
    },
  },
} satisfies Meta<typeof FileViewerStory>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Wait until Monaco has laid out the file, so captures show text. */
async function editorReady(canvasElement: HTMLElement) {
  await waitFor(
    () => {
      const lines = canvasElement.querySelector(".monaco-editor .view-lines");
      expect(lines?.textContent?.length ?? 0).toBeGreaterThan(20);
    },
    { timeout: 10_000 },
  );
}

export const ImageFile: Story = {};

export const ImageLoadFailure: Story = {
  args: { scenario: "image-failure" },
};

export const UnsupportedBinaryFile: Story = {
  args: { scenario: "unsupported" },
};

export const RetainedCheckpoint: Story = {
  args: { scenario: "retained" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByText(/from the checkpoint/),
    ).resolves.toBeVisible();
    await waitFor(() => {
      const editor = canvasElement.querySelector(".monaco-editor");
      expect(editor?.getBoundingClientRect().height ?? 0).toBeGreaterThan(100);
    });
  },
};

export const LiveCheckpoint: Story = {
  args: { scenario: "live" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.findByText(/^Live · saved/)).resolves.toBeVisible();
  },
};

/** The path keeps the row; the chip and editor button wrap under it. */
export const LiveCheckpointNarrow: Story = {
  args: { scenario: "live" },
  decorators: [
    (Story) => (
      <div className="w-[358px]">
        <Story />
      </div>
    ),
  ],
};

export const RetainedCheckpointNarrow: Story = {
  args: { scenario: "retained" },
  decorators: [
    (Story) => (
      <div className="w-[358px]">
        <Story />
      </div>
    ),
  ],
};

export const Loading: Story = {
  args: { scenario: "loading" },
};

export const SandboxUnavailable: Story = {
  args: { scenario: "unavailable" },
};

export const CompactImageFile: Story = {
  globals: { viewport: { value: "compact", isRotated: false } },
};

/** A text file before you choose Edit. */
export const ReadOnlyTextFile: Story = {
  args: { scenario: "read-only" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByRole("button", { name: "Edit" }),
    ).resolves.toBeVisible();
    await editorReady(canvasElement);
  },
};

/** Typed into and not saved: the header says so and offers Save. */
export const EditingWithUnsavedChanges: Story = {
  args: { scenario: "editing" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.findByText("Unsaved changes")).resolves.toBeVisible();
    await editorReady(canvasElement);
  },
};

export const Saving: Story = {
  args: { scenario: "saving" },
  play: async ({ canvasElement }) => {
    await editorReady(canvasElement);
  },
};

export const Saved: Story = {
  args: { scenario: "saved" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(canvas.findByText("Saved")).resolves.toBeVisible();
    await editorReady(canvasElement);
  },
};

export const SaveFailed: Story = {
  args: { scenario: "save-failed" },
  play: async ({ canvasElement }) => {
    await editorReady(canvasElement);
  },
};

/** A save the server refused because an agent changed the file first. */
export const Conflict: Story = {
  args: { scenario: "conflict" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByRole("button", { name: "Overwrite" }),
    ).resolves.toBeVisible();
    await editorReady(canvasElement);
  },
};

/** Compare opens the diff between the version on disk and yours. */
export const ConflictComparison: Story = {
  args: { scenario: "conflict" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Compare" }),
    );
    await waitFor(
      () => {
        const diff = canvasElement.querySelector(".monaco-diff-editor");
        expect(diff?.getBoundingClientRect().height ?? 0).toBeGreaterThan(100);
      },
      { timeout: 10_000 },
    );
  },
};

/** A reload found the file changed under your unsaved buffer. */
export const ChangedOnDisk: Story = {
  args: { scenario: "changed-on-disk" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByText("This file changed on disk."),
    ).resolves.toBeVisible();
    await editorReady(canvasElement);
  },
};

/** Markdown opens rendered; the Preview toggle shows the source. */
export const MarkdownPreview: Story = {
  args: { scenario: "markdown-preview" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByRole("heading", { name: "Editing files" }),
    ).resolves.toBeVisible();
  },
};

/** The preview renders the buffer, unsaved changes included. */
export const MarkdownPreviewWhileEditing: Story = {
  args: { scenario: "markdown-editing" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await userEvent.click(
      await canvas.findByRole("button", { name: "Preview" }),
    );
    await expect(
      canvas.findByText("Markdown files also get a preview."),
    ).resolves.toBeVisible();
  },
};

/** A file the viewer cut short opens read-only and says why. */
export const EditUnavailable: Story = {
  args: { scenario: "edit-unavailable" },
  play: async ({ canvasElement }) => {
    const canvas = within(canvasElement);
    await expect(
      canvas.findByText("Files over 512 KB are read-only"),
    ).resolves.toBeVisible();
    await editorReady(canvasElement);
  },
};

/** A sandbox workspace: the page says so before the file loads. */
export const EditUnavailableInSandbox: Story = {
  args: { scenario: "read-only", pageReason: "Sandbox files are read-only" },
};
