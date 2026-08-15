// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeComposer } from "./CodeComposer";
import { PERMISSION_MODE_UNAVAILABLE_REASON } from "./labels";

afterEach(() => {
  cleanup();
});

describe("CodeComposer", () => {
  it("enables Ask and Auto and reports the selected mode", () => {
    const onPermissionMode = vi.fn();
    render(
      <CodeComposer
        running={false}
        permissionMode="ask"
        onPermissionMode={onPermissionMode}
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "Plan" })).toBeEnabled();
    const ask = screen.getByRole("button", { name: "Ask" });
    const auto = screen.getByRole("button", { name: "Auto" });
    expect(ask).toBeEnabled();
    expect(auto).toBeEnabled();
    expect(ask).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(auto);
    expect(onPermissionMode).toHaveBeenCalledWith("auto");
  });

  it("disables a mode the harness cannot honor", () => {
    render(
      <CodeComposer
        running={false}
        permissionMode="plan"
        availableModes={["plan"]}
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );
    const ask = screen.getByRole("button", { name: /Ask/ });
    expect(ask).toBeDisabled();
    expect(ask).toHaveAttribute(
      "title",
      `Ask: ${PERMISSION_MODE_UNAVAILABLE_REASON}`,
    );
  });

  it("keeps the draft when send is refused", async () => {
    const onSend = vi.fn().mockRejectedValue(new Error("session is fenced"));
    render(
      <CodeComposer
        running={false}
        permissionMode="plan"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );
    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "list the files" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    await waitFor(() => expect(onSend).toHaveBeenCalled());
    expect(box).toHaveValue("list the files");
  });
});
