// @vitest-environment jsdom

import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { DeliverablePreview, OutputRevisionInfo } from "@/deliverables";
import { renderWithRouter } from "@/test/router";
import { OutputDetailRoot, type OutputDetailApis } from "./OutputDetailRoot";

afterEach(cleanup);

const outputId = "550062d4-2528-5cc6-90f8-a788e119bf36";
const revisionId = "72cb0277-5a3c-45ee-bda8-43534f74feb2";
/** An output id this conversation does not own. */
const absentOutputId = "ce116263-15b5-5df2-b472-269378e9da58";

function preview(overrides: Partial<DeliverablePreview> = {}): DeliverablePreview {
  return {
    outputId,
    filename: "Research brief.md",
    mediaType: "text/markdown",
    revisionCount: 1,
    content: "# Findings\n\nGrounded.",
    truncated: false,
    ...overrides,
  };
}

const olderRevisionId = "0e44560b-5d3b-4f80-b24c-647560f7ef19";

function revisionRow(overrides: Partial<OutputRevisionInfo> = {}): OutputRevisionInfo {
  return {
    revisionId,
    ordinal: 2,
    sizeBytes: 42,
    createdAt: "2026-07-24T00:00:00Z",
    producedBy: "agent",
    isCurrent: true,
    ...overrides,
  };
}

function detailApis(overrides: Partial<OutputDetailApis> = {}): OutputDetailApis {
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
        revisionRow(),
        revisionRow({
          revisionId: olderRevisionId,
          ordinal: 1,
          isCurrent: false,
        }),
      ],
    }),
    readRevision: vi.fn().mockResolvedValue(
      preview({ revisionCount: 2, content: "# Findings\n\nEarlier draft." }),
    ),
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

async function openOutput(apis: OutputDetailApis, id = outputId) {
  return renderWithRouter(
    <OutputDetailRoot chatId="chat-1" outputId={id} position="left" apis={apis} />,
    { initialUrl: `/c/chat-1?left=outputs.${id}&right=chat` },
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
    expect(await screen.findByRole("heading", { name: "Findings" })).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Save as…" }));
    await waitFor(() => expect(apis.export).toHaveBeenCalledWith("chat-1", outputId));
    expect(await screen.findByText("Research brief.md was saved.")).toBeVisible();
  });

  it("leads back to the list", async () => {
    const user = userEvent.setup();
    const { router } = await openOutput(detailApis());
    await screen.findByRole("heading", { name: "Findings" });

    await user.click(screen.getByRole("button", { name: "Outputs" }));
    await waitFor(() =>
      expect(router.state.location.search).toEqual({ left: "outputs", right: "chat" }),
    );
  });

  it("says so when the conversation does not have that output", async () => {
    const apis = detailApis({
      read: vi.fn().mockRejectedValue(new Error("output not found")),
    });
    await openOutput(apis, absentOutputId);

    expect(await screen.findByRole("alert")).toHaveTextContent("output not found");
  });

  // Non-markdown outputs are source text, not prose: rendering a CSV as markdown
  // would eat its structure.
  it("renders a delimited output as text rather than as markdown", async () => {
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

    expect(await screen.findByText(/# not a heading,2/)).toBeVisible();
    expect(screen.queryByRole("heading", { name: "not a heading,2" })).toBeNull();
    expect(screen.getByText(/Saving writes the complete file/)).toBeVisible();
  });

  // Alpha parity: the history affordance is invisible until an output actually
  // has history, so a first version can never offer a restore.
  it("hides version history when the output has a single version", async () => {
    const apis = detailApis();
    await openOutput(apis);
    await screen.findByRole("heading", { name: "Findings" });

    expect(
      screen.queryByRole("button", { name: "Version history" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Revert/ })).not.toBeInTheDocument();
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

    await user.click(screen.getByRole("button", { name: /v1/ }));
    await waitFor(() =>
      expect(apis.readRevision).toHaveBeenCalledWith("chat-1", outputId, olderRevisionId),
    );
    expect(await screen.findByText(/Viewing v1/)).toBeVisible();
    expect(await screen.findByText("Earlier draft.")).toBeVisible();
    // Export is scoped to the latest version, so it pauses while previewing.
    expect(screen.getByRole("button", { name: "Save as…" })).toBeDisabled();

    await user.click(screen.getByRole("button", { name: "Restore this version" }));
    await waitFor(() =>
      expect(apis.restoreRevision).toHaveBeenCalledWith(
        "chat-1",
        outputId,
        olderRevisionId,
      ),
    );
    // Back at the latest version, which now carries the restored content.
    expect(await screen.findByText(/Restored version 1 as the latest/)).toBeVisible();
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

  // Binary artifacts arrive with empty content; the panel offers export rather
  // than an inline rendering it cannot produce.
  it("offers export instead of a preview for a binary artifact", async () => {
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

    expect(
      await screen.findByText(/No preview for this file type/),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Save as…" })).toBeEnabled();
  });
});
