// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { ShortcutsList } from "./ShortcutsDialog";

afterEach(cleanup);

describe("ShortcutsList", () => {
  it("lists the diff's file keys in code mode", () => {
    render(<ShortcutsList mode="code" command />);
    const next = screen.getByText("Next file");
    expect(next.nextElementSibling).toHaveTextContent("J");
    const previous = screen.getByText("Previous file");
    expect(previous.nextElementSibling).toHaveTextContent("K");
    expect(
      screen.getByText("Comment on the selected lines"),
    ).toBeInTheDocument();
  });

  it("leaves them out of chat, which has no diff", () => {
    render(<ShortcutsList mode="chat" command={false} />);
    expect(screen.queryByText("Next file")).toBeNull();
  });
});
