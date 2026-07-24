import { describe, expect, it } from "vitest";
import {
  parseDeliverablePreview,
  parseDeliverablesCatalog,
} from "./deliverables";

describe("deliverable renderer projections", () => {
  it("accepts only the bounded closed catalog", () => {
    expect(
      parseDeliverablesCatalog({
        deliverables: [
          {
            filename: "Research brief.md",
            mediaType: "text/markdown",
            sizeBytes: 42,
            updatedAt: "2026-07-24T00:00:00Z",
          },
        ],
        truncated: false,
      }),
    ).toEqual({
      deliverables: [
        {
          filename: "Research brief.md",
          mediaType: "text/markdown",
          sizeBytes: 42,
          updatedAt: "2026-07-24T00:00:00Z",
        },
      ],
      truncated: false,
    });
  });

  it.each([
    "../escape.md",
    "/tmp/private.md",
    ".hidden.md",
    "report.pdf",
    "bad\u{202e}.md",
  ])("rejects unsafe filename %s", (filename) => {
    expect(() =>
      parseDeliverablesCatalog({
        deliverables: [
          {
            filename,
            mediaType: "text/markdown",
            sizeBytes: 1,
            updatedAt: "2026-07-24T00:00:00Z",
          },
        ],
        truncated: false,
      }),
    ).toThrow("Invalid output response");
  });

  it("rejects canonical paths and oversized previews", () => {
    expect(() =>
      parseDeliverablePreview({
        filename: "brief.md",
        mediaType: "text/markdown",
        content: "safe",
        truncated: false,
        path: "/private/scratch/brief.md",
      }),
    ).toThrow("Invalid output preview response");
    expect(() =>
      parseDeliverablePreview({
        filename: "brief.md",
        mediaType: "text/markdown",
        content: "x".repeat(100_001),
        truncated: true,
      }),
    ).toThrow("Invalid output preview response");
  });
});
