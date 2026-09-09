// @vitest-environment jsdom
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";

import { Composer } from "./Composer";
import { useUiStore } from "./UiStore";

afterEach(() => {
  cleanup();
  useUiStore.setState({ activeTurnSendMode: "queue" });
});

beforeEach(() => {
  useUiStore.setState({ activeTurnSendMode: "queue" });
});

function ActiveComposer({
  onSend,
  onSteer,
  onQueue,
  active = true,
}: {
  onSend: () => Promise<void>;
  onSteer: () => Promise<void>;
  onQueue?: () => Promise<void>;
  active?: boolean;
}) {
  const [draft, setDraft] = useState("redirect the turn");
  return (
    <Composer
      activeTurnId={active ? "turn-1" : null}
      busy={active}
      cancelError={null}
      cancelPending={false}
      disabled={false}
      draft={draft}
      onDraftChange={setDraft}
      onSend={onSend}
      onSteer={onSteer}
      onQueue={onQueue}
      onStop={async () => undefined}
      resetKey="chat-1"
      steerError={null}
      steerPending={false}
      steerStatus={null}
    />
  );
}

describe("Composer Cmd/Ctrl+Enter steer", () => {
  it("queues on Enter and steers on Cmd+Enter while queue is the default", async () => {
    const onSteer = vi.fn().mockResolvedValue(undefined);
    const onQueue = vi.fn().mockResolvedValue(undefined);
    const onSend = vi.fn().mockResolvedValue(undefined);
    render(
      <ActiveComposer onSend={onSend} onSteer={onSteer} onQueue={onQueue} />,
    );

    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.keyDown(box, { key: "Enter" });
    await waitFor(() => expect(onQueue).toHaveBeenCalledTimes(1));
    expect(onSteer).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();

    fireEvent.keyDown(box, { key: "Enter", metaKey: true });
    await waitFor(() => expect(onSteer).toHaveBeenCalledTimes(1));
    expect(onQueue).toHaveBeenCalledTimes(1);
    expect(onSend).not.toHaveBeenCalled();
  });

  it("steers on Ctrl+Enter as well as Cmd+Enter", async () => {
    const onSteer = vi.fn().mockResolvedValue(undefined);
    const onQueue = vi.fn().mockResolvedValue(undefined);
    render(
      <ActiveComposer onSend={vi.fn()} onSteer={onSteer} onQueue={onQueue} />,
    );

    fireEvent.keyDown(screen.getByRole("textbox", { name: "Message" }), {
      key: "Enter",
      ctrlKey: true,
    });
    await waitFor(() => expect(onSteer).toHaveBeenCalledTimes(1));
    expect(onQueue).not.toHaveBeenCalled();
  });

  it("does not send or steer on Cmd+Enter when no turn is running", async () => {
    const onSteer = vi.fn().mockResolvedValue(undefined);
    const onSend = vi.fn().mockResolvedValue(undefined);
    render(<ActiveComposer active={false} onSend={onSend} onSteer={onSteer} />);

    fireEvent.keyDown(screen.getByRole("textbox", { name: "Message" }), {
      key: "Enter",
      metaKey: true,
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(onSteer).not.toHaveBeenCalled();
    expect(onSend).not.toHaveBeenCalled();
  });

  it("ignores Cmd+Enter during IME composition and with Shift", async () => {
    const onSteer = vi.fn().mockResolvedValue(undefined);
    render(
      <ActiveComposer onSend={vi.fn()} onSteer={onSteer} onQueue={vi.fn()} />,
    );
    const box = screen.getByRole("textbox", { name: "Message" });

    fireEvent.keyDown(box, {
      key: "Enter",
      metaKey: true,
      isComposing: true,
      keyCode: 229,
    });
    fireEvent.keyDown(box, { key: "Enter", metaKey: true, shiftKey: true });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(onSteer).not.toHaveBeenCalled();
  });

  it("documents the steer chord on the queue action tooltip", () => {
    render(
      <ActiveComposer onSend={vi.fn()} onSteer={vi.fn()} onQueue={vi.fn()} />,
    );
    expect(
      screen.getByRole("button", {
        name: "Queue message for after this response",
      }),
    ).toBeVisible();
  });
});
