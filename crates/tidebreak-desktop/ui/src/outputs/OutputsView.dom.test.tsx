// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { OutputsView, type OutputsApis } from "./OutputsView";

afterEach(() => {
  cleanup();
});

describe("OutputsView", () => {
  it("does not show the empty catalog under a load failure", async () => {
    const apis: OutputsApis = {
      list: async () => {
        throw new Error("The output catalog did not respond.");
      },
      export: async () => {
        throw new Error("unused");
      },
      delete: async () => {
        throw new Error("unused");
      },
      restore: async () => {
        throw new Error("unused");
      },
    };

    render(<OutputsView chatId="chat-story" apis={apis} />);

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "The output catalog did not respond.",
      ),
    );
    expect(screen.queryByText("No outputs yet")).not.toBeInTheDocument();
  });
});
