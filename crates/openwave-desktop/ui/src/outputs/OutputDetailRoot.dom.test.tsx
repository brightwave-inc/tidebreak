// @vitest-environment jsdom

import { cleanup, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { DeliverablePreview } from "@/deliverables";
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
    content: "# Findings\n\nGrounded.",
    truncated: false,
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
    revert: vi.fn().mockResolvedValue({
      status: "retracted" as const,
      outputId,
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
});
