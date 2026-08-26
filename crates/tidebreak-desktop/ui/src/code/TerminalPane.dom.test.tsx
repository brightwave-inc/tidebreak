// @vitest-environment jsdom
import type { ComponentProps } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { TerminalPane } from "./TerminalPane";

const written: string[] = [];
const terminals: Array<{
  disposed: boolean;
  options: {
    theme?: Record<string, string>;
    disableStdin?: boolean;
  };
  dataHandler: ((data: string) => void) | null;
  write: (data: string, cb?: () => void) => void;
}> = [];

vi.mock("@xterm/xterm", () => ({
  Terminal: class MockTerminal {
    cols = 80;
    rows = 24;
    disposed = false;
    options: {
      theme?: Record<string, string>;
      disableStdin?: boolean;
    };
    dataHandler: ((data: string) => void) | null = null;
    write(data: string, cb?: () => void) {
      written.push(data);
      cb?.();
    }
    loadAddon() {}
    open() {}
    dispose() {
      this.disposed = true;
    }
    onData(handler: (data: string) => void) {
      this.dataHandler = handler;
      return { dispose() {} };
    }
    constructor(options?: {
      theme?: Record<string, string>;
      disableStdin?: boolean;
    }) {
      this.options = { ...options };
      terminals.push(this);
    }
  },
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class {
    fit() {}
  },
}));

vi.mock("@xterm/xterm/css/xterm.css", () => ({}));

afterEach(() => {
  cleanup();
  written.length = 0;
  terminals.length = 0;
  vi.restoreAllMocks();
  document.documentElement.classList.remove("dark");
});

function encode(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function snapshot(id = "term-1", ended = false) {
  return {
    id,
    workspace_id: "ws-1",
    cols: 80,
    rows: 24,
    ended,
    created_at: "2026-08-25T12:00:00.000Z",
  };
}

function readPage(
  id = "term-1",
  overrides: Partial<{
    bytes: string;
    cursor: number;
    overflow: boolean;
    truncated: boolean;
    ended: boolean;
  }> = {},
) {
  return {
    id,
    workspace_id: "ws-1",
    bytes: "",
    cursor: 0,
    overflow: false,
    truncated: false,
    ended: false,
    ...overrides,
  };
}

type TerminalClient = ComponentProps<typeof TerminalPane>["client"];

function clientWith(overrides: Partial<TerminalClient> = {}): TerminalClient {
  return {
    listCodeTerminals: vi.fn().mockResolvedValue([snapshot()]),
    createCodeTerminal: vi.fn().mockResolvedValue(snapshot("term-new")),
    readCodeTerminal: vi.fn().mockResolvedValue(readPage()),
    writeCodeTerminal: vi.fn().mockResolvedValue(undefined),
    resizeCodeTerminal: vi.fn().mockResolvedValue(snapshot()),
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function expectReady() {
  await waitFor(() =>
    expect(screen.getByTestId("terminal-host")).toHaveAttribute(
      "aria-disabled",
      "false",
    ),
  );
}

function emitData(data: string, index = terminals.length - 1) {
  const handler = terminals[index]?.dataHandler;
  if (!handler) throw new Error(`Terminal ${index} has no data handler`);
  act(() => handler(data));
}

async function rejectWrite(
  pending: ReturnType<typeof deferred<void>>,
  message = "Terminal write failed",
) {
  await act(async () => {
    pending.reject(new Error(message));
    await Promise.resolve();
  });
}

describe("TerminalPane", () => {
  it("disposes the renderer when the pane unmounts", async () => {
    const client = clientWith();
    const { unmount } = render(
      <TerminalPane client={client} workspaceId="ws-1" />,
    );
    await waitFor(() => expect(client.readCodeTerminal).toHaveBeenCalled());
    expect(terminals).toHaveLength(1);
    expect(terminals[0]?.disposed).toBe(false);
    unmount();
    expect(terminals[0]?.disposed).toBe(true);
  });

  it("renders a truncation notice when the ring overflowed", async () => {
    const client = clientWith({
      readCodeTerminal: vi.fn().mockResolvedValue(
        readPage("term-1", {
          bytes: encode("\r\n[output truncated]\r\nlater"),
          cursor: 12,
          overflow: true,
        }),
      ),
    });
    render(<TerminalPane client={client} workspaceId="ws-1" />);
    expect(await screen.findByTestId("terminal-truncated")).toHaveTextContent(
      "Output was truncated.",
    );
    expect(screen.getByRole("group", { name: "Terminal output" })).toBe(
      screen.getByTestId("terminal-host"),
    );
  });

  it("says the shell is opening until it answers, then gets out of the way", async () => {
    const pendingRead = deferred<ReturnType<typeof readPage>>();
    const client = clientWith({
      listCodeTerminals: vi.fn().mockResolvedValue([]),
      createCodeTerminal: vi.fn().mockResolvedValue(snapshot()),
      readCodeTerminal: vi.fn().mockReturnValue(pendingRead.promise),
    });
    render(<TerminalPane client={client} workspaceId="ws-1" />);
    expect(await screen.findByTestId("terminal-starting")).toHaveTextContent(
      "Opening a shell in the worktree…",
    );

    await act(async () => pendingRead.resolve(readPage()));
    await waitFor(() =>
      expect(screen.queryByTestId("terminal-starting")).toBeNull(),
    );
  });

  it("renders the ended-shell state", async () => {
    const client = clientWith({
      listCodeTerminals: vi.fn().mockResolvedValue([]),
      createCodeTerminal: vi.fn().mockResolvedValue(snapshot("term-1", true)),
      readCodeTerminal: vi
        .fn()
        .mockResolvedValue(readPage("term-1", { ended: true })),
    });
    render(<TerminalPane client={client} workspaceId="ws-1" />);
    expect(await screen.findByTestId("terminal-ended")).toHaveTextContent(
      "Shell ended.",
    );
    expect(screen.getByTestId("terminal-host")).toHaveAttribute(
      "aria-disabled",
      "true",
    );
  });

  it("serializes writes while an earlier request is delayed", async () => {
    const first = deferred<void>();
    const writeCodeTerminal = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockResolvedValue(undefined);
    const client = clientWith({ writeCodeTerminal });
    render(
      <TerminalPane client={client} workspaceId="ws-1" terminalId="term-1" />,
    );
    await expectReady();

    emitData("first");
    emitData("second");
    expect(writeCodeTerminal).toHaveBeenCalledTimes(1);
    expect(writeCodeTerminal).toHaveBeenNthCalledWith(
      1,
      "ws-1",
      "term-1",
      "first",
    );

    await act(async () => first.resolve(undefined));
    await waitFor(() => expect(writeCodeTerminal).toHaveBeenCalledTimes(2));
    expect(writeCodeTerminal).toHaveBeenNthCalledWith(
      2,
      "ws-1",
      "term-1",
      "second",
    );
  });

  it("preserves all unsent bytes and freezes input after a write failure", async () => {
    const first = deferred<void>();
    const writeCodeTerminal = vi.fn().mockReturnValue(first.promise);
    const client = clientWith({ writeCodeTerminal });
    render(
      <TerminalPane client={client} workspaceId="ws-1" terminalId="term-1" />,
    );
    await expectReady();

    emitData("echo ");
    emitData("safe\n");
    await rejectWrite(first);

    const failure = await screen.findByTestId("terminal-write-failure");
    expect(failure).toHaveTextContent("10 unsent bytes");
    expect(failure).toHaveTextContent("Terminal write failed");
    expect(screen.getByTestId("terminal-host")).toHaveAttribute(
      "aria-disabled",
      "true",
    );
    expect(terminals.at(-1)?.options.disableStdin).toBe(true);

    emitData("ignored");
    expect(writeCodeTerminal).toHaveBeenCalledTimes(1);
    expect(failure).toHaveTextContent("10 unsent bytes");
  });

  it("retries the failed chunk before later retained chunks", async () => {
    const first = deferred<void>();
    const writeCodeTerminal = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockResolvedValue(undefined);
    const client = clientWith({ writeCodeTerminal });
    render(
      <TerminalPane client={client} workspaceId="ws-1" terminalId="term-1" />,
    );
    await expectReady();

    emitData("a");
    emitData("β");
    await rejectWrite(first);
    expect(
      await screen.findByTestId("terminal-write-failure"),
    ).toHaveTextContent("3 unsent bytes");

    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(writeCodeTerminal).toHaveBeenCalledTimes(3));
    expect(writeCodeTerminal.mock.calls.map((call) => call[2])).toEqual([
      "a",
      "a",
      "β",
    ]);
    expect(screen.queryByTestId("terminal-write-failure")).toBeNull();
    expect(screen.getByTestId("terminal-host")).toHaveAttribute(
      "aria-disabled",
      "false",
    );
  });

  it("keeps failed input retryable when the parent adopts the attached terminal id", async () => {
    const first = deferred<void>();
    const writeCodeTerminal = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockResolvedValue(undefined);
    const client = clientWith({ writeCodeTerminal });
    const { rerender } = render(
      <TerminalPane client={client} workspaceId="ws-1" />,
    );
    await expectReady();

    emitData("retained");
    await rejectWrite(first);
    await screen.findByTestId("terminal-write-failure");
    rerender(
      <TerminalPane client={client} workspaceId="ws-1" terminalId="term-1" />,
    );

    await waitFor(() =>
      expect(client.readCodeTerminal).toHaveBeenCalledTimes(2),
    );
    expect(screen.getByTestId("terminal-write-failure")).toHaveTextContent(
      "8 unsent bytes",
    );
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));

    await waitFor(() => expect(writeCodeTerminal).toHaveBeenCalledTimes(2));
    expect(writeCodeTerminal.mock.calls.map((call) => call[2])).toEqual([
      "retained",
      "retained",
    ]);
    expect(screen.queryByTestId("terminal-write-failure")).toBeNull();
  });

  it("discards retained bytes and continues in the same shell", async () => {
    const first = deferred<void>();
    const writeCodeTerminal = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockResolvedValue(undefined);
    const client = clientWith({ writeCodeTerminal });
    render(
      <TerminalPane client={client} workspaceId="ws-1" terminalId="term-1" />,
    );
    await expectReady();

    emitData("failed");
    emitData("queued");
    await rejectWrite(first);
    await screen.findByTestId("terminal-write-failure");

    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    emitData("after");
    await waitFor(() => expect(writeCodeTerminal).toHaveBeenCalledTimes(2));
    expect(writeCodeTerminal.mock.calls.map((call) => call[2])).toEqual([
      "failed",
      "after",
    ]);
    expect(writeCodeTerminal.mock.calls[1]?.[1]).toBe("term-1");
  });

  it("reconnects to a fresh shell after discarding retained bytes", async () => {
    const first = deferred<void>();
    const writeCodeTerminal = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockResolvedValue(undefined);
    const createCodeTerminal = vi.fn().mockResolvedValue(snapshot("term-2"));
    const readCodeTerminal = vi.fn(
      async (_workspaceId: string, terminalId: string) => readPage(terminalId),
    );
    const onAttach = vi.fn();
    const client = clientWith({
      createCodeTerminal,
      readCodeTerminal,
      writeCodeTerminal,
    });
    render(
      <TerminalPane client={client} workspaceId="ws-1" onAttach={onAttach} />,
    );
    await expectReady();

    emitData("failed");
    emitData("queued");
    await rejectWrite(first);
    await screen.findByTestId("terminal-write-failure");
    fireEvent.click(screen.getByRole("button", { name: "Reconnect" }));

    await waitFor(() => expect(createCodeTerminal).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(onAttach).toHaveBeenCalledWith("term-2"));
    await expectReady();
    emitData("new-shell");
    await waitFor(() => expect(writeCodeTerminal).toHaveBeenCalledTimes(2));
    expect(writeCodeTerminal.mock.calls).toEqual([
      ["ws-1", "term-1", "failed"],
      ["ws-1", "term-2", "new-shell"],
    ]);
  });

  it("retries an attach failure", async () => {
    const listCodeTerminals = vi
      .fn()
      .mockRejectedValueOnce(new Error("Could not list terminals"))
      .mockResolvedValue([snapshot()]);
    const client = clientWith({ listCodeTerminals });
    render(<TerminalPane client={client} workspaceId="ws-1" />);

    const failure = await screen.findByTestId("terminal-attach-error");
    expect(failure).toHaveTextContent("Input stays paused");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));

    await waitFor(() => expect(listCodeTerminals).toHaveBeenCalledTimes(2));
    await expectReady();
    expect(screen.queryByTestId("terminal-attach-error")).toBeNull();
  });

  it("retries a read failure without replacing the shell", async () => {
    const readCodeTerminal = vi
      .fn()
      .mockRejectedValueOnce(new Error("Terminal output was unavailable"))
      .mockResolvedValue(readPage());
    const client = clientWith({ readCodeTerminal });
    render(
      <TerminalPane client={client} workspaceId="ws-1" terminalId="term-1" />,
    );

    const failure = await screen.findByTestId("terminal-read-error");
    expect(failure).toHaveTextContent("Input is paused");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));

    await waitFor(() => expect(readCodeTerminal).toHaveBeenCalledTimes(2));
    await expectReady();
    expect(client.listCodeTerminals).not.toHaveBeenCalled();
    expect(client.createCodeTerminal).not.toHaveBeenCalled();
    expect(screen.queryByTestId("terminal-read-error")).toBeNull();
  });

  it("fences a stale data handler when the terminal identity changes", async () => {
    const client = clientWith({
      readCodeTerminal: vi.fn(
        async (_workspaceId: string, terminalId: string) =>
          readPage(terminalId),
      ),
    });
    const { rerender } = render(
      <TerminalPane client={client} workspaceId="ws-1" terminalId="term-a" />,
    );
    await expectReady();
    const staleTerminal = terminals[0];

    rerender(
      <TerminalPane client={client} workspaceId="ws-1" terminalId="term-b" />,
    );
    act(() => staleTerminal?.dataHandler?.("stale"));
    await waitFor(() =>
      expect(client.readCodeTerminal).toHaveBeenCalledWith("ws-1", "term-b", 0),
    );
    await expectReady();
    emitData("current");

    await waitFor(() =>
      expect(client.writeCodeTerminal).toHaveBeenCalledOnce(),
    );
    expect(client.writeCodeTerminal).toHaveBeenCalledWith(
      "ws-1",
      "term-b",
      "current",
    );
  });

  it("abandons queued input when the workspace changes during a delayed write", async () => {
    const first = deferred<void>();
    const nextWorkspaceTerminals = deferred<ReturnType<typeof snapshot>[]>();
    const listCodeTerminals = vi.fn((workspaceId: string) =>
      workspaceId === "ws-a"
        ? Promise.resolve([snapshot("term-a")])
        : nextWorkspaceTerminals.promise,
    );
    const writeCodeTerminal = vi
      .fn()
      .mockImplementationOnce(() => first.promise)
      .mockResolvedValue(undefined);
    const client = clientWith({
      listCodeTerminals,
      readCodeTerminal: vi.fn(
        async (_workspaceId: string, terminalId: string) =>
          readPage(terminalId),
      ),
      writeCodeTerminal,
    });
    const { rerender } = render(
      <TerminalPane client={client} workspaceId="ws-a" />,
    );
    await expectReady();

    emitData("in-flight");
    emitData("must-not-cross");
    rerender(<TerminalPane client={client} workspaceId="ws-b" />);
    await act(async () => first.resolve(undefined));
    expect(writeCodeTerminal).toHaveBeenCalledTimes(1);

    await act(async () => nextWorkspaceTerminals.resolve([snapshot("term-b")]));
    await expectReady();
    emitData("current");

    await waitFor(() => expect(writeCodeTerminal).toHaveBeenCalledTimes(2));
    expect(writeCodeTerminal.mock.calls).toEqual([
      ["ws-a", "term-a", "in-flight"],
      ["ws-b", "term-b", "current"],
    ]);
  });

  it("ignores a delayed read from the preceding terminal identity", async () => {
    const firstRead = deferred<ReturnType<typeof readPage>>();
    const readCodeTerminal = vi.fn(
      async (_workspaceId: string, terminalId: string) => {
        if (terminalId === "term-a") return firstRead.promise;
        return readPage("term-b", { bytes: encode("current output") });
      },
    );
    const client = clientWith({ readCodeTerminal });
    const { rerender } = render(
      <TerminalPane client={client} workspaceId="ws-1" terminalId="term-a" />,
    );
    await waitFor(() =>
      expect(readCodeTerminal).toHaveBeenCalledWith("ws-1", "term-a", 0),
    );

    rerender(
      <TerminalPane client={client} workspaceId="ws-1" terminalId="term-b" />,
    );
    await waitFor(() => expect(written).toContain("current output"));
    await act(async () =>
      firstRead.resolve(
        readPage("term-a", { bytes: encode("preceding output") }),
      ),
    );

    expect(written).not.toContain("preceding output");
  });

  it("does not finish a pending attach retry after unmount", async () => {
    const retryList = deferred<ReturnType<typeof snapshot>[]>();
    const listCodeTerminals = vi
      .fn()
      .mockRejectedValueOnce(new Error("Could not list terminals"))
      .mockReturnValueOnce(retryList.promise);
    const createCodeTerminal = vi.fn().mockResolvedValue(snapshot());
    const client = clientWith({ listCodeTerminals, createCodeTerminal });
    const { unmount } = render(
      <TerminalPane client={client} workspaceId="ws-1" />,
    );

    await screen.findByTestId("terminal-attach-error");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(listCodeTerminals).toHaveBeenCalledTimes(2));
    unmount();
    await act(async () => retryList.resolve([]));

    expect(createCodeTerminal).not.toHaveBeenCalled();
    expect(terminals.every((terminal) => terminal.disposed)).toBe(true);
  });

  it("builds the xterm theme from CSS tokens and reapplies it when the root class flips", async () => {
    const light: Record<string, string> = {
      "--background": "#fafafa",
      "--foreground": "#18181b",
      "--muted-foreground": "#71717a",
      "--success": "#16a34a",
      "--success-foreground": "#14532d",
      "--warning": "#ca8a04",
      "--warning-foreground": "#713f12",
      "--critical": "#dc2626",
      "--critical-foreground": "#7f1d1d",
      "--info": "#2563eb",
      "--info-foreground": "#1e3a8a",
    };
    const dark: Record<string, string> = {
      "--background": "#18181b",
      "--foreground": "#e4e4e7",
      "--muted-foreground": "#a1a1aa",
      "--success": "#4ade80",
      "--success-foreground": "#bbf7d0",
      "--warning": "#facc15",
      "--warning-foreground": "#fef08a",
      "--critical": "#f87171",
      "--critical-foreground": "#fecaca",
      "--info": "#60a5fa",
      "--info-foreground": "#bfdbfe",
    };
    const original = window.getComputedStyle;
    vi.spyOn(window, "getComputedStyle").mockImplementation((element) => {
      if (element === document.documentElement) {
        const tokens = document.documentElement.classList.contains("dark")
          ? dark
          : light;
        return {
          getPropertyValue: (name: string) => tokens[name] ?? "",
        } as CSSStyleDeclaration;
      }
      return original.call(window, element);
    });

    const client = clientWith();
    document.documentElement.classList.remove("dark");
    render(<TerminalPane client={client} workspaceId="ws-1" />);
    await waitFor(() => expect(client.readCodeTerminal).toHaveBeenCalled());

    expect(screen.getByTestId("terminal-host")).toHaveAttribute(
      "aria-label",
      "Terminal output",
    );
    expect(terminals[0]?.options.theme).toMatchObject({
      background: "#fafafa",
      foreground: "#18181b",
      red: "#dc2626",
      green: "#16a34a",
      yellow: "#ca8a04",
      blue: "#2563eb",
    });

    document.documentElement.classList.add("dark");
    await waitFor(() =>
      expect(terminals[0]?.options.theme).toMatchObject({
        background: "#18181b",
        foreground: "#e4e4e7",
        red: "#f87171",
        green: "#4ade80",
      }),
    );
  });
});
