// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "sonner";

import { Toaster } from "@/components/ui/sonner";
import { persistErrorToasts } from "./errorToasts";

beforeEach(() => {
  persistErrorToasts();
});

afterEach(() => {
  cleanup();
  toast.dismiss();
  vi.useRealTimers();
});

describe("error toasts", () => {
  it("keeps an error toast until it is dismissed", {
    timeout: 10_000,
  }, async () => {
    render(<Toaster richColors />);
    toast.error("Could not save");

    expect(await screen.findByText("Could not save")).toBeTruthy();
    await new Promise((resolve) => setTimeout(resolve, 4500));
    expect(screen.getByText("Could not save")).toBeTruthy();

    await userEvent.click(screen.getByRole("button", { name: /close/i }));
    await waitFor(() => {
      expect(screen.queryByText("Could not save")).toBeNull();
    });
  });
});
