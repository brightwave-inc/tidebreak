// @vitest-environment jsdom

import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { DeliverablePreview, OutputRevisionInfo } from "@/deliverables";
import { Toaster } from "@/components/ui/sonner";
import { renderWithRouter } from "@/test/router";
import { SourceNavProvider, type SourceNav } from "@/panel/SourceNav";
import { OutputDetailRoot, type OutputDetailApis } from "./OutputDetailRoot";

const hostMocks = vi.hoisted(() => ({ openExternal: vi.fn() }));
vi.mock("@/host", async () => {
  const actual = await vi.importActual<typeof import("@/host")>("@/host");
  return { ...actual, openExternal: hostMocks.openExternal };
});

vi.mock("@/components/document/image-viewer", () => ({
  ImageViewer: () => <div>Image preview</div>,
}));

vi.mock("@/document/DocumentViewer", async () => {
  const actual = await vi.importActual<
    typeof import("@/document/DocumentViewer")
  >("@/document/DocumentViewer");
  return {
    ...actual,
    DocumentViewer: ({ mediaType }: { mediaType: string }) => (
      <div>Document preview ({mediaType})</div>
    ),
  };
});

afterEach(() => {
  cleanup();
  hostMocks.openExternal.mockReset();
});

const outputId = "550062d4-2528-5cc6-90f8-a788e119bf36";
const revisionId = "72cb0277-5a3c-45ee-bda8-43534f74feb2";
const citationId = "46abf484-8368-4c2d-b2ec-8b9ed77e202f";
const documentId = "4571ebc0-69a7-4f8a-a9c7-936c50f0f022";
/** An output id this conversation does not own. */
const absentOutputId = "ce116263-15b5-5df2-b472-269378e9da58";

function preview(
  overrides: Partial<DeliverablePreview> = {},
): DeliverablePreview {
  return {
    outputId,
    filename: "Research brief.md",
    mediaType: "text/markdown",
    revisionCount: 1,
    revisionId,
    content: "# Findings\n\nGrounded.",
    truncated: false,
    ...overrides,
  };
}

const olderRevisionId = "0e44560b-5d3b-4f80-b24c-647560f7ef19";

function revisionRow(
  overrides: Partial<OutputRevisionInfo> = {},
): OutputRevisionInfo {
  return {
    revisionId,
    ordinal: 2,
    sizeBytes: 42,
    createdAt: "2026-07-24T00:00:00Z",
    producedBy: "agent",
    isCurrent: true,
    sources: [],
    ...overrides,
  };
}

function detailApis(
  overrides: Partial<OutputDetailApis> = {},
): OutputDetailApis {
  return {
    read: vi.fn().mockResolvedValue(preview()),
    export: vi.fn().mockResolvedValue({
      operationId: "0e44560b-5d3b-4f80-b24c-647560f7ef19",
      outputId,
      revisionId,
      status: "completed" as const,
    }),
    listRevisions: vi.fn().mockResolvedValue({
      outputId,
      revisions: [
        revisionRow({ producedBy: "backgroundAgent" }),
        revisionRow({
          revisionId: olderRevisionId,
          ordinal: 1,
          isCurrent: false,
        }),
      ],
    }),
    readRevision: vi
      .fn()
      .mockResolvedValue(
        preview({ revisionCount: 2, content: "# Findings\n\nEarlier draft." }),
      ),
    save: vi.fn().mockResolvedValue({ status: "saved", preview: preview() }),
    restoreRevision: vi.fn().mockResolvedValue({
      outputId,
      filename: "Research brief.md",
      mediaType: "text/markdown",
      sizeBytes: 42,
      revisionCount: 3,
      updatedAt: "2026-07-24T00:00:00Z",
      producingRunId: null,
    }),
    ...overrides,
  };
}

async function openOutput(
  apis: OutputDetailApis,
  id = outputId,
  sourceNav: SourceNav | null = null,
) {
  return renderWithRouter(
    <>
      <SourceNavProvider value={sourceNav}>
        <OutputDetailRoot chatId="chat-1" outputId={id} apis={apis} />
      </SourceNavProvider>
      <Toaster richColors />
    </>,
    { initialUrl: `/c/chat-1?tabs=outputs.${id}` },
  );
}

describe("OutputDetailRoot", () => {
  // The address is the selection: this panel reads what it was asked for, which
  // is what removed the list's old habit of steering its own selection.
  it("reads the output its address names, renders it, and saves it", async () => {
    const apis = detailApis();
    const user = userEvent.setup();
    await openOutput(apis);

    expect(apis.read).toHaveBeenCalledWith("chat-1", outputId);
    expect(
      await screen.findByRole("heading", { name: "Findings" }),
    ).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Save as…" }));
    await waitFor(() =>
      expect(apis.export).toHaveBeenCalledWith("chat-1", outputId),
    );
    expect(
      await screen.findByText("Research brief.md was saved."),
    ).toBeVisible();
  });

  it("leads back to the list", async () => {
    const user = userEvent.setup();
    const { router } = await openOutput(detailApis());
    await screen.findByRole("heading", { name: "Findings" });

    await user.click(screen.getByRole("button", { name: "Outputs" }));
    await waitFor(() =>
      expect(router.state.location.search).toEqual({ tabs: "outputs" }),
    );
  });

  it("says so when the conversation does not have that output", async () => {
    const apis = detailApis({
      read: vi.fn().mockRejectedValue(new Error("output not found")),
    });
    await openOutput(apis, absentOutputId);

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "output not found",
    );
  });

  // CSV shares the spreadsheet viewer with xlsx — never markdown, which would
  // eat its structure into headings.
  it("renders a delimited output in the spreadsheet viewer", async () => {
    const apis = detailApis({
      read: vi.fn().mockResolvedValue(
        preview({
          filename: "Totals.csv",
          mediaType: "text/csv",
          content: "# not a heading,2\nrow,3",
          truncated: true,
        }),
      ),
    });
    await openOutput(apis);

    expect(
      await screen.findByText("Document preview (text/csv)"),
    ).toBeVisible();
    expect(
      screen.queryByRole("heading", { name: "not a heading,2" }),
    ).toBeNull();
  });

  // The history affordance stays hidden until an output actually has history,
  // so a first version can never offer a restore.
  it("hides version history when the output has a single version", async () => {
    const apis = detailApis();
    await openOutput(apis);
    await screen.findByRole("heading", { name: "Findings" });

    expect(
      screen.queryByRole("button", { name: "Version history" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Revert/ }),
    ).not.toBeInTheDocument();
  });

  it("shows the exact revision's document and web sources", async () => {
    hostMocks.openExternal.mockResolvedValue(true);
    const oldAgentRevisionId = "56c62f92-05d2-4830-8664-75dab007d087";
    const userRevisionId = "8ac32ae1-c55f-4c11-820b-873912d144c2";
    const apis = detailApis({
      read: vi.fn().mockResolvedValue(preview({ revisionCount: 3 })),
      listRevisions: vi.fn().mockResolvedValue({
        outputId,
        revisions: [
          revisionRow({
            ordinal: 3,
            sources: [
              {
                kind: "document",
                citationId,
                documentId,
                locator: { kind: "lines", start: 4, end: 8 },
              },
              {
                kind: "web",
                url: "https://example.com/current",
                label: "Current research",
                domain: "example.com",
              },
            ],
          }),
          revisionRow({
            revisionId: oldAgentRevisionId,
            ordinal: 2,
            isCurrent: false,
            sources: [
              {
                kind: "web",
                url: "https://archive.example/earlier",
                label: "Earlier research",
                domain: "archive.example",
              },
            ],
          }),
          revisionRow({
            revisionId: userRevisionId,
            ordinal: 1,
            producedBy: "user",
            isCurrent: false,
            sources: [],
          }),
        ],
      }),
    });
    const openCitation = vi.fn();
    const user = userEvent.setup();
    await openOutput(apis, outputId, {
      openCitation,
      openDocument: vi.fn(),
    });

    expect(await screen.findByLabelText("Output sources")).toBeVisible();
    expect(screen.getByText("Lines 4–8")).toBeVisible();
    expect(screen.getByText("example.com")).toBeVisible();
    await user.click(
      screen.getByRole("button", { name: "Open document source 1" }),
    );
    expect(openCitation).toHaveBeenCalledWith({ documentId, citationId });
    await user.click(screen.getByRole("button", { name: "Current research" }));
    expect(hostMocks.openExternal).toHaveBeenCalledWith(
      "https://example.com/current",
    );

    await user.click(screen.getByRole("button", { name: "Version history" }));
    await user.click(await screen.findByRole("button", { name: /v2/ }));
    expect(await screen.findByText("archive.example")).toBeVisible();
    expect(screen.queryByText("example.com")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Version history" }));
    await user.click(await screen.findByRole("button", { name: /v1/ }));
    await screen.findByText(/Viewing v1/);
    expect(screen.queryByLabelText("Output sources")).not.toBeInTheDocument();
  });

  it("previews an old version and restores it as a new latest version", async () => {
    const apis = detailApis({
      read: vi.fn().mockResolvedValue(preview({ revisionCount: 2 })),
    });
    const user = userEvent.setup();
    await openOutput(apis);
    await screen.findByRole("heading", { name: "Findings" });

    await user.click(screen.getByRole("button", { name: "Version history" }));
    await waitFor(() =>
      expect(apis.listRevisions).toHaveBeenCalledWith("chat-1", outputId),
    );
    // The current version is labeled and offers no restore of its own.
    expect(await screen.findByText("Current version")).toBeVisible();
    // A shared filename can carry versions from different producers; the
    // history keeps that merge visible rather than presenting one anonymous
    // stream of revisions.
    expect(screen.getByText(/^Background agent ·/)).toBeVisible();
    expect(screen.getByText(/^Agent ·/)).toBeVisible();

    await user.click(screen.getByRole("button", { name: /v1/ }));
    await waitFor(() =>
      expect(apis.readRevision).toHaveBeenCalledWith(
        "chat-1",
        outputId,
        olderRevisionId,
      ),
    );
    expect(await screen.findByText(/Viewing v1/)).toBeVisible();
    expect(await screen.findByText("Earlier draft.")).toBeVisible();
    // Export is scoped to the latest version, so it pauses while previewing.
    expect(screen.getByRole("button", { name: "Save as…" })).toBeDisabled();

    await user.click(
      screen.getByRole("button", { name: "Restore this version" }),
    );
    await waitFor(() =>
      expect(apis.restoreRevision).toHaveBeenCalledWith(
        "chat-1",
        outputId,
        olderRevisionId,
      ),
    );
    // Back at the latest version, which now carries the restored content.
    expect(
      await screen.findByText(/Restored version 1 as the latest/),
    ).toBeVisible();
    expect(screen.queryByText(/Viewing v1/)).not.toBeInTheDocument();
  });

  it("leaves version preview without restoring via back to latest", async () => {
    const apis = detailApis({
      read: vi.fn().mockResolvedValue(preview({ revisionCount: 2 })),
    });
    const user = userEvent.setup();
    await openOutput(apis);
    await screen.findByRole("heading", { name: "Findings" });

    await user.click(screen.getByRole("button", { name: "Version history" }));
    await user.click(await screen.findByRole("button", { name: /v1/ }));
    await screen.findByText(/Viewing v1/);

    await user.click(screen.getByRole("button", { name: "Back to latest" }));
    expect(screen.queryByText(/Viewing v1/)).not.toBeInTheDocument();
    expect(apis.restoreRevision).not.toHaveBeenCalled();
  });

  it("draws an image output with the shared image viewer", async () => {
    const apis = detailApis({
      read: vi.fn().mockResolvedValue(
        preview({
          filename: "chart.png",
          mediaType: "image/png",
          content: "",
        }),
      ),
    });
    await openOutput(apis);

    expect(await screen.findByText("Image preview")).toBeVisible();
    expect(screen.getByRole("button", { name: "Save as…" })).toBeEnabled();
  });

  it("draws an office output with the shared document viewer", async () => {
    const apis = detailApis({
      read: vi.fn().mockResolvedValue(
        preview({
          filename: "model.xlsx",
          mediaType:
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
          content: "",
        }),
      ),
    });
    await openOutput(apis);

    expect(
      await screen.findByText(
        "Document preview (application/vnd.openxmlformats-officedocument.spreadsheetml.sheet)",
      ),
    ).toBeVisible();
  });

  it("draws a source-code output in the syntax-highlighted viewer", async () => {
    const apis = detailApis({
      read: vi.fn().mockResolvedValue(
        preview({
          filename: "solution.py",
          mediaType: "text/plain",
          content: "def greet():\n    return 'hello'",
        }),
      ),
    });
    await openOutput(apis);

    await waitFor(() =>
      expect(document.querySelector(".language-python")).not.toBeNull(),
    );
    expect(screen.queryByText(/No preview for this file type/)).toBeNull();
  });

  // Formats with no viewer still arrive with empty content; the panel offers
  // export rather than an inline rendering it cannot produce.
  it("offers export instead of a preview for an unsupported binary artifact", async () => {
    const apis = detailApis({
      read: vi.fn().mockResolvedValue(
        preview({
          filename: "bundle.zip",
          mediaType: "application/zip",
          content: "",
        }),
      ),
    });
    await openOutput(apis);

    expect(
      await screen.findByText(/No preview for this file type/),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Save as…" })).toBeEnabled();
  });
});
