import { describe, expect, it } from "vitest";

import { exportFailureMessage } from "./OutputsView";

describe("exportFailureMessage", () => {
  it("names why a save failed instead of claiming it was saved", () => {
    expect(exportFailureMessage("source_unavailable")).toBe(
      "That output revision is no longer available.",
    );
    expect(exportFailureMessage("destination_unavailable")).toBe(
      "The selected save destination is no longer available.",
    );
    expect(exportFailureMessage("ambiguous_native_failure")).toBe(
      "Tidebreak could not confirm whether the output was saved. Check the selected destination before trying again.",
    );
  });
});
