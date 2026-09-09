// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { clearFileDownloadCache } from "@/document/useFileDownload";
import { FileViewer, imageMediaTypeForPath } from "./FileViewer";

vi.mock("@monaco-editor/react", () => ({
  default: () => <div>Text editor</div>,
  loader: { config: vi.fn() },
}));

beforeEach(() => {
  clearFileDownloadCache();
  URL.createObjectURL = vi.fn(() => "blob:workspace-image");
  URL.revokeObjectURL = vi.fn();
});

afterEach(cleanup);

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
  });
});

it("recognizes browser image extensions without case sensitivity", () => {
  expect(imageMediaTypeForPath("assets/PHOTO.JPEG")).toBe("image/jpeg");
  expect(imageMediaTypeForPath("assets/diagram.svg")).toBe("image/svg+xml");
  expect(imageMediaTypeForPath("assets/archive.zip")).toBeNull();
});
