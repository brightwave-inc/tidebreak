// @vitest-environment jsdom

import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DeliverablesView } from "./DeliverablesView";

afterEach(cleanup);

const firstOutputId = "550062d4-2528-5cc6-90f8-a788e119bf36";
const secondOutputId = "ce116263-15b5-5df2-b472-269378e9da58";
const revisionId = "72cb0277-5a3c-45ee-bda8-43534f74feb2";

describe("DeliverablesView", () => {
  it("previews and explicitly exports a conversation output", async () => {
    const exportOutput = vi.fn().mockResolvedValue({
      operationId: "0e44560b-5d3b-4f80-b24c-647560f7ef19",
      outputId: firstOutputId,
      revisionId,
      status: "completed" as const,
    });
    const apis = {
      list: vi.fn().mockResolvedValue({
        deliverables: [
          {
            outputId: firstOutputId,
            filename: "Research brief.md",
            mediaType: "text/markdown" as const,
            sizeBytes: 42,
            revisionCount: 1,
            updatedAt: "2026-07-24T00:00:00Z",
          },
        ],
        truncated: false,
      }),
      read: vi.fn().mockResolvedValue({
        outputId: firstOutputId,
        filename: "Research brief.md",
        mediaType: "text/markdown" as const,
        content: "# Findings\n\nGrounded.",
        truncated: false,
      }),
      export: exportOutput,
    };

    render(
      <DeliverablesView chatId="chat-1" apis={apis} />,
    );
    expect(await screen.findByRole("heading", { name: "Findings" })).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: /Save As/ }));
    await waitFor(() =>
      expect(exportOutput).toHaveBeenCalledWith("chat-1", firstOutputId),
    );
    expect(await screen.findByText("Research brief.md was saved.")).toBeVisible();
  });

  it("previews the output it was pointed at, not the first one", async () => {
    const apis = twoOutputApis();

    render(
      <DeliverablesView
        chatId="chat-1"
        initialOutputId={secondOutputId}
        apis={apis}
      />,
    );

    await waitFor(() =>
      expect(apis.read).toHaveBeenCalledWith("chat-1", secondOutputId),
    );
    expect(apis.read).not.toHaveBeenCalledWith("chat-1", firstOutputId);
  });

  it("falls back to the list when it is pointed at an output this chat lacks", async () => {
    const apis = twoOutputApis();

    render(
      <DeliverablesView
        chatId="chat-1"
        initialOutputId="550062d4-2528-5cc6-90f8-a788e119bf37"
        apis={apis}
      />,
    );

    await waitFor(() =>
      expect(apis.read).toHaveBeenCalledWith("chat-1", firstOutputId),
    );
    expect(apis.read).not.toHaveBeenCalledWith(
      "chat-1",
      "550062d4-2528-5cc6-90f8-a788e119bf37",
    );
  });

  it("stops steering once the reader chooses another output", async () => {
    const apis = twoOutputApis();

    render(
      <DeliverablesView
        chatId="chat-1"
        initialOutputId={secondOutputId}
        apis={apis}
      />,
    );
    await waitFor(() =>
      expect(apis.read).toHaveBeenCalledWith("chat-1", secondOutputId),
    );

    await userEvent.click(screen.getByRole("button", { name: /First\.md/ }));
    await waitFor(() =>
      expect(apis.read).toHaveBeenCalledWith("chat-1", firstOutputId),
    );

    await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(apis.list).toHaveBeenCalledTimes(2));
    expect(apis.read).toHaveBeenLastCalledWith("chat-1", firstOutputId);
  });

  it("explains how to create the first output", async () => {
    render(
      <DeliverablesView
        chatId="chat-1"
        apis={{
          list: vi.fn().mockResolvedValue({
            deliverables: [],
            truncated: false,
          }),
          read: vi.fn(),
          export: vi.fn(),
        }}
      />,
    );
    expect(await screen.findByText("No outputs yet")).toBeVisible();
    expect(
      screen.getByText(/Ask OpenWave to create a report/),
    ).toBeVisible();
  });

  it("presents an export failure as an alert", async () => {
    const apis = {
      list: vi.fn().mockResolvedValue({
        deliverables: [
          {
            outputId: firstOutputId,
            filename: "brief.txt",
            mediaType: "text/plain" as const,
            sizeBytes: 5,
            revisionCount: 1,
            updatedAt: "2026-07-24T00:00:00Z",
          },
        ],
        truncated: false,
      }),
      read: vi.fn().mockResolvedValue({
        outputId: firstOutputId,
        filename: "brief.txt",
        mediaType: "text/plain" as const,
        content: "brief",
        truncated: false,
      }),
      export: vi.fn().mockResolvedValue({
        operationId: "0e44560b-5d3b-4f80-b24c-647560f7ef19",
        outputId: firstOutputId,
        revisionId,
        status: "failed" as const,
        reason: "ambiguous_native_failure" as const,
      }),
    };

    render(
      <DeliverablesView chatId="chat-1" apis={apis} />,
    );
    await screen.findByText("brief");
    await userEvent.click(screen.getByRole("button", { name: /Save As/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "could not confirm whether the output was saved",
    );
  });

  it("keeps the refreshed preview when an older read completes later", async () => {
    let resolveStalePreview:
      | ((preview: {
          outputId: string;
          filename: string;
          mediaType: "text/plain";
          content: string;
          truncated: boolean;
        }) => void)
      | undefined;
    const stalePreview = new Promise<{
      outputId: string;
      filename: string;
      mediaType: "text/plain";
      content: string;
      truncated: boolean;
    }>((resolve) => {
      resolveStalePreview = resolve;
    });
    const apis = {
      list: vi.fn().mockResolvedValue({
        deliverables: [
          {
            outputId: firstOutputId,
            filename: "status.txt",
            mediaType: "text/plain" as const,
            sizeBytes: 7,
            revisionCount: 1,
            updatedAt: "2026-07-24T00:00:00Z",
          },
        ],
        truncated: false,
      }),
      read: vi
        .fn()
        .mockReturnValueOnce(stalePreview)
        .mockResolvedValueOnce({
          filename: "status.txt",
          outputId: firstOutputId,
          mediaType: "text/plain" as const,
          content: "refreshed",
          truncated: false,
        }),
      export: vi.fn(),
    };

    render(
      <DeliverablesView chatId="chat-1" apis={apis} />,
    );
    await waitFor(() => expect(apis.read).toHaveBeenCalledTimes(1));
    await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(await screen.findByText("refreshed")).toBeVisible();

    await act(async () => {
      resolveStalePreview?.({
        outputId: firstOutputId,
        filename: "status.txt",
        mediaType: "text/plain",
        content: "stale",
        truncated: false,
      });
    });
    expect(screen.getByText("refreshed")).toBeVisible();
    expect(screen.queryByText("stale")).not.toBeInTheDocument();
  });
});

function twoOutputApis() {
  const outputs = [
    { outputId: firstOutputId, filename: "First.md" },
    { outputId: secondOutputId, filename: "Second.md" },
  ];
  return {
    list: vi.fn().mockResolvedValue({
      deliverables: outputs.map(({ outputId, filename }) => ({
        outputId,
        filename,
        mediaType: "text/markdown" as const,
        sizeBytes: 12,
        revisionCount: 1,
        updatedAt: "2026-07-24T00:00:00Z",
      })),
      truncated: false,
    }),
    read: vi.fn().mockImplementation((_chatId: string, outputId: string) => {
      const filename =
        outputs.find((output) => output.outputId === outputId)?.filename ?? "Missing";
      return Promise.resolve({
        outputId,
        filename,
        mediaType: "text/markdown" as const,
        content: `# ${filename}`,
        truncated: false,
      });
    }),
    export: vi.fn().mockResolvedValue({
      operationId: "0e44560b-5d3b-4f80-b24c-647560f7ef19",
      outputId: firstOutputId,
      revisionId,
      status: "completed" as const,
    }),
  };
}
