// @vitest-environment jsdom

import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { NetworkPolicyDialog } from "./NetworkPolicyDialog";

afterEach(cleanup);

describe("NetworkPolicyDialog", () => {
  it("selects a package-registry policy without applying it until submit", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <NetworkPolicyDialog
        open
        onOpenChange={vi.fn()}
        value={{ mode: "off" }}
        onChange={onChange}
      />,
    );

    await user.click(screen.getByRole("radio", { name: /Package installs/i }));
    expect(onChange).not.toHaveBeenCalled();
    await user.click(
      screen.getByRole("button", { name: "Apply network policy" }),
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
    expect(onChange).not.toHaveBeenCalled();
    fireEvent.click(
      screen.getByRole("button", { name: "Apply network policy" }),
    );

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

    const internetAccess = screen.getByRole("radio", {
      name: /Internet access/i,
    });
    expect(screen.getByRole("button", { name: "Close" })).toBeInTheDocument();

    fireEvent.click(internetAccess);
    expect(onChange).not.toHaveBeenCalled();
    const apply = screen.getByRole("button", { name: "Apply network policy" });
    fireEvent.click(apply);
    fireEvent.click(apply);

    expect(onChange).toHaveBeenCalledTimes(1);
    expect(internetAccess).toBeDisabled();
    expect(
      screen.queryByRole("button", { name: "Close" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent(
      "Saving network policy",
    );

    await act(async () => {
      rejectUpdate(new Error("The network policy could not be saved."));
    });

    expect(screen.getByRole("alert")).toHaveTextContent(
      "The network policy could not be saved.",
    );
    expect(internetAccess).not.toBeDisabled();
    expect(screen.getByRole("button", { name: "Close" })).toBeInTheDocument();
    expect(onOpenChange).not.toHaveBeenCalledWith(false);
  });
});
