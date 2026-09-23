// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiClient } from "@/api";
import { InstructionsPanel } from "./InstructionsPanel";

function client(stored = "Answer in British English.") {
  let current = stored;
  return {
    getPersonalInstructions: vi.fn(async () => ({ instructions: current })),
    putPersonalInstructions: vi.fn(async (instructions: string) => {
      current = instructions;
      return { instructions };
    }),
  };
}

function renderPanel(api: ReturnType<typeof client>) {
  return render(<InstructionsPanel client={api as unknown as ApiClient} />);
}

afterEach(cleanup);

describe("Personal instructions", () => {
  it("saves when you leave the field, and only when the text changed", async () => {
    const api = client();
    const user = userEvent.setup();
    renderPanel(api);
    const field = await screen.findByRole("textbox", {
      name: "Personal instructions",
    });
    expect(field).toHaveProperty("value", "Answer in British English.");

    await user.click(field);
    await user.tab();
    expect(api.putPersonalInstructions).not.toHaveBeenCalled();

    await user.clear(field);
    await user.type(field, "Lead with the answer.");
    await user.tab();
    await waitFor(() =>
      expect(api.putPersonalInstructions).toHaveBeenCalledWith(
        "Lead with the answer.",
      ),
    );
    expect(api.putPersonalInstructions).toHaveBeenCalledTimes(1);
  });

  it("counts bytes near the cap and refuses to save past it", async () => {
    const api = client("");
    renderPanel(api);
    const field = await screen.findByRole("textbox", {
      name: "Personal instructions",
    });

    fireEvent.change(field, { target: { value: "a".repeat(6000) } });
    expect(screen.queryByText(/of 8,192 bytes/)).toBeNull();

    // Two bytes a character: 4,000 characters are 8,000 bytes.
    fireEvent.change(field, { target: { value: "é".repeat(4000) } });
    expect(screen.getByText("8,000 of 8,192 bytes")).toBeTruthy();
    expect(field.getAttribute("aria-invalid")).toBeNull();

    fireEvent.change(field, { target: { value: "é".repeat(4097) } });
    expect(screen.getByText("8,194 of 8,192 bytes")).toBeTruthy();
    expect(field.getAttribute("aria-invalid")).toBe("true");
    expect(
      screen
        .getByRole("alert")
        .textContent?.includes("Shorten the instructions"),
    ).toBe(true);
    fireEvent.blur(field);
    await Promise.resolve();
    expect(api.putPersonalInstructions).not.toHaveBeenCalled();

    fireEvent.change(field, { target: { value: "a".repeat(8192) } });
    fireEvent.blur(field);
    await waitFor(() =>
      expect(api.putPersonalInstructions).toHaveBeenCalledWith(
        "a".repeat(8192),
      ),
    );
  });

  it("keeps your text and says why when a save fails", async () => {
    const api = client();
    api.putPersonalInstructions.mockRejectedValueOnce(
      new Error("The server is unavailable."),
    );
    const user = userEvent.setup();
    renderPanel(api);
    const field = await screen.findByRole("textbox", {
      name: "Personal instructions",
    });
    await user.clear(field);
    await user.type(field, "Keep it short.");
    await user.tab();
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      "The server is unavailable.",
    );
    expect(field).toHaveProperty("value", "Keep it short.");
  });

  it("saves an edit that is still in the field when the page closes", async () => {
    const api = client();
    const view = renderPanel(api);
    const field = await screen.findByRole("textbox", {
      name: "Personal instructions",
    });
    fireEvent.change(field, { target: { value: "Use metric units." } });
    view.unmount();
    expect(api.putPersonalInstructions).toHaveBeenCalledWith(
      "Use metric units.",
    );
  });

  it("offers a retry when the instructions do not load", async () => {
    const api = client();
    api.getPersonalInstructions.mockRejectedValueOnce(
      new Error("The server is unavailable."),
    );
    const user = userEvent.setup();
    renderPanel(api);
    await user.click(await screen.findByRole("button", { name: "Try again" }));
    expect(
      await screen.findByRole("textbox", { name: "Personal instructions" }),
    ).toHaveProperty("value", "Answer in British English.");
  });
});
