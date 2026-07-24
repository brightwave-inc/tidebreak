// @vitest-environment jsdom

import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DeliverablesView } from "./DeliverablesView";

afterEach(cleanup);

describe("DeliverablesView", () => {
  it("previews and explicitly exports a conversation output", async () => {
    const exportOutput = vi.fn().mockResolvedValue(true);
    const apis = {
      list: vi.fn().mockResolvedValue({
        deliverables: [
          {
            filename: "Research brief.md",
            mediaType: "text/markdown" as const,
            sizeBytes: 42,
            updatedAt: "2026-07-24T00:00:00Z",
          },
        ],
        truncated: false,
      }),
      read: vi.fn().mockResolvedValue({
        filename: "Research brief.md",
        mediaType: "text/markdown" as const,
        content: "# Findings\n\nGrounded.",
        truncated: false,
      }),
      export: exportOutput,
    };

    render(
      <DeliverablesView chatId="chat-1" onBack={() => {}} apis={apis} />,
    );
    expect(await screen.findByRole("heading", { name: "Findings" })).toBeVisible();
    await userEvent.click(screen.getByRole("button", { name: /Save As/ }));
    await waitFor(() =>
      expect(exportOutput).toHaveBeenCalledWith("chat-1", "Research brief.md"),
    );
    expect(await screen.findByText("Research brief.md was saved.")).toBeVisible();
  });

  it("explains how to create the first output", async () => {
    render(
      <DeliverablesView
        chatId="chat-1"
        onBack={() => {}}
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
            filename: "brief.txt",
            mediaType: "text/plain" as const,
            sizeBytes: 5,
            updatedAt: "2026-07-24T00:00:00Z",
          },
        ],
        truncated: false,
      }),
      read: vi.fn().mockResolvedValue({
        filename: "brief.txt",
        mediaType: "text/plain" as const,
        content: "brief",
        truncated: false,
      }),
      export: vi.fn().mockRejectedValue(new Error("Selected folder is unavailable")),
    };

    render(
      <DeliverablesView chatId="chat-1" onBack={() => {}} apis={apis} />,
    );
    await screen.findByText("brief");
    await userEvent.click(screen.getByRole("button", { name: /Save As/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Selected folder is unavailable",
    );
  });

  it("keeps the refreshed preview when an older read completes later", async () => {
    let resolveStalePreview:
      | ((preview: {
          filename: string;
          mediaType: "text/plain";
          content: string;
          truncated: boolean;
        }) => void)
      | undefined;
    const stalePreview = new Promise<{
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
            filename: "status.txt",
            mediaType: "text/plain" as const,
            sizeBytes: 7,
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
          mediaType: "text/plain" as const,
          content: "refreshed",
          truncated: false,
        }),
      export: vi.fn(),
    };

    render(
      <DeliverablesView chatId="chat-1" onBack={() => {}} apis={apis} />,
    );
    await waitFor(() => expect(apis.read).toHaveBeenCalledTimes(1));
    await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
    expect(await screen.findByText("refreshed")).toBeVisible();

    await act(async () => {
      resolveStalePreview?.({
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
