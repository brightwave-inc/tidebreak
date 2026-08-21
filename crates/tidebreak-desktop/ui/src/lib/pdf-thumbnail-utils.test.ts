import { describe, expect, it } from "vitest";

import { resolvePdfiumWorkerAssetUrl } from "@/lib/pdf-thumbnail-utils";

describe("resolvePdfiumWorkerAssetUrl", () => {
  it("makes a Vite asset URL absolute before it crosses into a blob worker", () => {
    expect(
      resolvePdfiumWorkerAssetUrl(
        "/assets/pdfium.wasm",
        "http://127.0.0.1:1420/documents/one",
      ),
    ).toBe("http://127.0.0.1:1420/assets/pdfium.wasm");
  });
});
