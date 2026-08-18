// @vitest-environment jsdom
import { useState } from "react";
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { SegmentedControl } from "./segmented";

afterEach(() => {
  cleanup();
});

function Switch() {
  const [value, setValue] = useState<"chat" | "code">("chat");
  return (
    <SegmentedControl
      aria-label="App mode"
      value={value}
      onValueChange={setValue}
      options={[
        { value: "chat", label: "Chat" },
        { value: "code", label: "Code" },
      ]}
    />
  );
}

describe("SegmentedControl", () => {
  it("moves the checked segment with arrow keys", async () => {
    const user = userEvent.setup();
    render(<Switch />);

    const chat = screen.getByRole("radio", { name: "Chat" });
    const code = screen.getByRole("radio", { name: "Code" });
    expect(chat).toHaveAttribute("aria-checked", "true");
    expect(code).toHaveAttribute("aria-checked", "false");

    chat.focus();
    await user.keyboard("{ArrowRight}");
    expect(code).toHaveAttribute("aria-checked", "true");
    expect(code).toHaveFocus();

    await user.keyboard("{ArrowLeft}");
    expect(chat).toHaveAttribute("aria-checked", "true");
    expect(chat).toHaveFocus();
  });
});
