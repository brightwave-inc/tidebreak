// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { NetworkPolicyDialog } from "./NetworkPolicyDialog";

afterEach(cleanup);

describe("NetworkPolicyDialog", () => {
  it("offers the package-registry class as one per-chat choice", () => {
    const onChange = vi.fn();
    render(
      <NetworkPolicyDialog
        open
        onOpenChange={vi.fn()}
        value={{ mode: "off" }}
        onChange={onChange}
      />,
    );

    fireEvent.click(
      screen
        .getByText(/Only curated package registries are reachable/i)
        .closest("button")!,
    );

    expect(onChange).toHaveBeenCalledWith({ mode: "package_managers" });
  });

  it("builds a deduplicated custom-host policy with optional registries", () => {
    const onChange = vi.fn();
    render(
      <NetworkPolicyDialog
        open
        onOpenChange={vi.fn()}
        value={{ mode: "off" }}
        onChange={onChange}
      />,
    );

    fireEvent.change(screen.getByLabelText("Allowed network hosts"), {
      target: { value: "api.example.com, api.example.com\nfiles.example.com" },
    });
    fireEvent.click(screen.getByText("Also allow package registries"));
    fireEvent.click(screen.getByRole("button", { name: "Use custom policy" }));

    expect(onChange).toHaveBeenCalledWith({
      mode: "allowed_hosts",
      allowed_hosts: ["api.example.com", "files.example.com"],
      package_managers: true,
    });
  });

  it("keeps the dialog open with an actionable error when saving fails", async () => {
    let rejectUpdate!: (reason?: unknown) => void;
    const update = new Promise<void>((_resolve, reject) => {
      rejectUpdate = reject;
    });
    const onChange = vi.fn(() => update);
    const onOpenChange = vi.fn();
    render(
      <NetworkPolicyDialog
        open
        onOpenChange={onOpenChange}
        value={{ mode: "off" }}
        onChange={onChange}
      />,
    );

    const internetAccess = screen
      .getByText(/Reach public internet destinations/i)
      .closest("button")!;
    fireEvent.click(internetAccess);
    fireEvent.click(internetAccess);

    expect(onChange).toHaveBeenCalledTimes(1);
    expect(internetAccess).toBeDisabled();
    expect(screen.getByRole("status")).toHaveTextContent("Saving network policy");

    await act(async () => {
      rejectUpdate(new Error("The network policy could not be saved."));
    });

    expect(screen.getByRole("alert")).toHaveTextContent(
      "The network policy could not be saved.",
    );
    expect(internetAccess).not.toBeDisabled();
    expect(onOpenChange).not.toHaveBeenCalledWith(false);
  });
});
