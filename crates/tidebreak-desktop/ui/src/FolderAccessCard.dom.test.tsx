// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { APPROVAL_SHORTCUT_GRACE_MS } from "./ApprovalChoiceList";
import { FolderAccessCard } from "./FolderAccessCard";

afterEach(() => {
  cleanup();
});

function card(overrides: Partial<Parameters<typeof FolderAccessCard>[0]> = {}) {
  return (
    <FolderAccessCard
      request={{
        callId: "call-folder",
        turnId: "turn-1",
        reason: "The agent wants to read files in a connected folder.",
        folderHint: "documents",
        claimedByDesktop: false,
      }}
      nativeHost
      nativeBusy={false}
      working={false}
      error={undefined}
      onDecision={vi.fn()}
      onCancel={vi.fn()}
      {...overrides}
    />
  );
}

function choices() {
  return screen
    .getAllByRole("button")
    .filter((button) => /^\d+\./.test(button.textContent ?? ""));
}

describe("FolderAccessCard", () => {
  it("declines when you press 2 then Enter", async () => {
    const user = userEvent.setup();
    const onDecision = vi.fn();
    render(card({ onDecision }));
    await new Promise((resolve) =>
      setTimeout(resolve, APPROVAL_SHORTCUT_GRACE_MS + 10),
    );

    await user.keyboard("2{Enter}");
    expect(onDecision).toHaveBeenCalledWith("decline");
  });

  it("does nothing when you press Enter during the grace period", async () => {
    const user = userEvent.setup();
    const onDecision = vi.fn();
    render(card({ onDecision }));

    await user.keyboard("{Enter}");
    expect(onDecision).not.toHaveBeenCalled();
  });

  it("allows when you click Allow this folder", async () => {
    const user = userEvent.setup();
    const onDecision = vi.fn();
    render(card({ onDecision }));

    await user.click(choices()[0]!);
    expect(onDecision).toHaveBeenCalledWith("allow");
  });
});
