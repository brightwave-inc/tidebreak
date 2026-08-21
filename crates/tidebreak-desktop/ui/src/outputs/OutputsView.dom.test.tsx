// @vitest-environment jsdom

import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { Toaster } from "@/components/ui/sonner";
import type { DeliverableSummary, DeliverablesCatalog } from "@/deliverables";
import { OutputsView, type OutputsApis } from "./OutputsView";

afterEach(cleanup);

const ids = {
  brief: "550062d4-2528-5cc6-90f8-a788e119bf36",
  sheet: "ce116263-15b5-5df2-b472-269378e9da58",
};
const revisionId = "72cb0277-5a3c-45ee-bda8-43534f74feb2";

function output(
  overrides: Partial<DeliverableSummary> = {},
): DeliverableSummary {
  return {
    outputId: ids.brief,
    filename: "Research brief.md",
    mediaType: "text/markdown",
    sizeBytes: 42,
    revisionCount: 1,
    updatedAt: "2026-07-24T00:00:00Z",
    producingRunId: null,
    ...overrides,
  };
}

function outputApis(deliverables: DeliverableSummary[]): OutputsApis {
  return {
    list: vi.fn().mockResolvedValue({ deliverables, truncated: false }),
    export: vi.fn().mockResolvedValue({
      operationId: "0e44560b-5d3b-4f80-b24c-647560f7ef19",
      outputId: ids.brief,
      revisionId,
      status: "completed" as const,
    }),
    delete: vi.fn().mockResolvedValue(output()),
    restore: vi.fn().mockResolvedValue(output()),
  };
}

describe("OutputsView", () => {
  it("searches the catalog and opens an output", async () => {
    const onOpen = vi.fn();
    const apis = outputApis([
      output(),
      output({
        outputId: ids.sheet,
        filename: "Totals.csv",
        mediaType: "text/csv",
        sizeBytes: 2_048,
        revisionCount: 3,
      }),
    ]);
    const user = userEvent.setup();

    render(<OutputsView chatId="chat-1" onOpen={onOpen} apis={apis} />);
    await screen.findByRole("button", { name: "Open Research brief.md" });
    expect(screen.getByText("CSV")).toBeVisible();
    expect(screen.getByText("2 KB")).toBeVisible();

    await user.type(screen.getByPlaceholderText("Search outputs…"), "csv");
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Open Research brief.md" }),
      ).not.toBeInTheDocument(),
    );
    expect(screen.getByText("showing 1 of 2")).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Open Totals.csv" }));
    expect(onOpen).toHaveBeenCalledWith(ids.sheet);
  });

  // Saving from the list matters because clearing a batch out to disk should not
  // mean opening each one first.
  it("saves an output from its row without opening it", async () => {
    const apis = outputApis([output()]);
    const user = userEvent.setup();

    render(
      <>
        <OutputsView chatId="chat-1" apis={apis} />
        <Toaster richColors />
      </>,
    );
    await user.click(
      await screen.findByRole("button", {
        name: "More options for Research brief.md",
      }),
    );
    await user.click(await screen.findByRole("menuitem", { name: "Save as…" }));

    await waitFor(() =>
      expect(apis.export).toHaveBeenCalledWith("chat-1", ids.brief),
    );
    expect(
      await screen.findByText("Research brief.md was saved."),
    ).toBeVisible();
  });

  it("deletes an output and offers an undo that restores it", async () => {
    const apis = outputApis([output({ producingRunId: ids.sheet })]);
    const user = userEvent.setup();

    render(
      <>
        <OutputsView chatId="chat-1" apis={apis} />
        <Toaster richColors />
      </>,
    );
    await user.click(
      await screen.findByRole("button", {
        name: "More options for Research brief.md",
      }),
    );
    await user.click(await screen.findByRole("menuitem", { name: "Delete" }));

    await waitFor(() =>
      expect(apis.delete).toHaveBeenCalledWith("chat-1", ids.brief),
    );
    expect(
      await screen.findByText(
        "Research brief.md was deleted from this conversation.",
      ),
    ).toBeVisible();

    // The delete is reversible: Undo restores the exact output.
    await user.click(screen.getByRole("button", { name: "Undo" }));
    await waitFor(() =>
      expect(apis.restore).toHaveBeenCalledWith("chat-1", ids.brief),
    );
    expect(
      await screen.findByText("Research brief.md was restored."),
    ).toBeVisible();
  });

  it("names the reason a save failed rather than reporting it as saved", async () => {
    const apis = outputApis([output()]);
    vi.mocked(apis.export).mockResolvedValue({
      operationId: "0e44560b-5d3b-4f80-b24c-647560f7ef19",
      outputId: ids.brief,
      revisionId,
      status: "failed",
      reason: "destination_unavailable",
    });
    const user = userEvent.setup();

    render(
      <>
        <OutputsView chatId="chat-1" apis={apis} />
        <Toaster richColors />
      </>,
    );
    await user.click(
      await screen.findByRole("button", {
        name: "More options for Research brief.md",
      }),
    );
    await user.click(await screen.findByRole("menuitem", { name: "Save as…" }));

    expect(
      await screen.findByText(/save destination is no longer available/),
    ).toBeVisible();
  });

  it("ignores a stale catalog response after the conversation changes", async () => {
    let resolveStale: ((catalog: DeliverablesCatalog) => void) | undefined;
    const stale = new Promise<DeliverablesCatalog>((resolve) => {
      resolveStale = resolve;
    });
    const apis = outputApis([]);
    vi.mocked(apis.list).mockImplementation((chatId) =>
      chatId === "chat-1"
        ? stale
        : Promise.resolve({
            deliverables: [
              output({ outputId: ids.sheet, filename: "Current.csv" }),
            ],
            truncated: false,
          }),
    );

    const view = render(
      <>
        <OutputsView chatId="chat-1" apis={apis} />
        <Toaster richColors />
      </>,
    );
    await waitFor(() => expect(apis.list).toHaveBeenCalledWith("chat-1"));

    view.rerender(<OutputsView chatId="chat-2" apis={apis} />);
    expect(
      await screen.findByRole("button", { name: "Open Current.csv" }),
    ).toBeVisible();
    await act(async () => {
      resolveStale?.({
        deliverables: [output({ filename: "Stale.md" })],
        truncated: false,
      });
    });
    expect(
      screen.getByRole("button", { name: "Open Current.csv" }),
    ).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Open Stale.md" }),
    ).not.toBeInTheDocument();
  });

  it("says how to make the first output when there are none", async () => {
    render(<OutputsView chatId="chat-1" apis={outputApis([])} />);
    expect(await screen.findByText("No outputs yet")).toBeVisible();
    expect(screen.getByText(/report, a plan, a CSV/i)).toBeVisible();
  });
});
