// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CodeComposer } from "./CodeComposer";
import { HttpError } from "../api/client";
import { PERMISSION_MODE_UNAVAILABLE_REASON } from "./labels";

afterEach(() => {
  cleanup();
});

const QUEUED = {
  kind: "queued" as const,
  queued: { session_id: "sess-1", message: "and run the tests", position: 1 },
};

describe("CodeComposer", () => {
  it("states the session's mode without offering to change it", () => {
    render(
      <CodeComposer
        running={false}
        permissionMode="ask"
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    const modes = screen.getByTestId("permission-modes");
    expect(modes).toHaveTextContent("Ask");
    expect(modes.querySelector("button")).toBeNull();
  });

  it("keeps an unavailable mode visible with the reason", () => {
    render(
      <CodeComposer
        running={false}
        permissionMode="plan"
        availableModes={["plan"]}
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );
    expect(screen.getByTitle(`Ask: ${PERMISSION_MODE_UNAVAILABLE_REASON}`)).toBeTruthy();
  });

  it("queues a follow-up written while a turn is running", async () => {
    const onSend = vi.fn().mockResolvedValue(QUEUED);
    render(
      <CodeComposer
        running
        permissionMode="ask"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "Interrupt" })).toBeEnabled();

    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "and run the tests" } });
    expect(screen.getByRole("button", { name: "Queue" })).toBeEnabled();
    fireEvent.keyDown(box, { key: "Enter" });

    await waitFor(() => expect(onSend).toHaveBeenCalledWith("and run the tests"));
    expect(box).toHaveValue("");
    expect(await screen.findByRole("status")).toHaveTextContent(
      "runs after the current turn",
    );
  });

  it("says the queue is full and keeps the draft", async () => {
    const onSend = vi
      .fn()
      .mockRejectedValue(
        new HttpError(409, "409: a follow-up is already queued", "queue_full"),
      );
    render(
      <CodeComposer
        running
        permissionMode="ask"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "and push it" } });
    fireEvent.click(screen.getByRole("button", { name: "Queue" }));

    expect(await screen.findByRole("status")).toHaveTextContent(
      "A follow-up is already queued",
    );
    expect(box).toHaveValue("and push it");
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
