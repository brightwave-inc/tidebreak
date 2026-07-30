// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { NetworkPolicyMenu } from "./NetworkPolicyMenu";

afterEach(cleanup);

describe("NetworkPolicyMenu", () => {
  it("offers the package-registry class as one per-chat choice", () => {
    const onChange = vi.fn();
    render(
      <NetworkPolicyMenu value={{ mode: "off" }} onChange={onChange} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Network: Network off" }));
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
      <NetworkPolicyMenu value={{ mode: "off" }} onChange={onChange} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Network: Network off" }));
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
});
