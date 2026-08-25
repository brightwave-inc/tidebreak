// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import {
  clearFileDownloadCache,
  type FileBytesSource,
} from "@/document/useFileDownload";
import { ImageViewer } from "./image-viewer";

function source(id: string): FileBytesSource {
  return {
    id,
    cacheKey: `image/${id}`,
    fetch: async () => ({
      bytes: new Uint8Array([id.length]),
      contentType: "image/png",
    }),
  };
}

beforeEach(() => {
  clearFileDownloadCache();
  let nextUrl = 0;
  URL.createObjectURL = vi.fn(() => `blob:image-${++nextUrl}`);
  URL.revokeObjectURL = vi.fn();
});

afterEach(cleanup);

it("reuses the shared URL lifecycle and resets render failures for a new source", async () => {
  const { rerender } = render(<ImageViewer source={source("one")} />);
  const firstImage = await screen.findByRole("img", {
    name: "Document image",
  });
  expect(firstImage).toHaveAttribute("src", "blob:image-1");

  fireEvent.error(firstImage);
  expect(screen.getByRole("alert")).toHaveTextContent(
    "This image could not be loaded.",
  );

  rerender(<ImageViewer source={source("two")} />);

  await waitFor(() =>
    expect(screen.getByRole("img", { name: "Document image" })).toHaveAttribute(
      "src",
      "blob:image-2",
    ),
  );
  expect(URL.revokeObjectURL).toHaveBeenCalledWith("blob:image-1");
});
