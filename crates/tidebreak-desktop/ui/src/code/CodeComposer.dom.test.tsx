// @vitest-environment jsdom
import {
  cleanup,
  createEvent,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { ReactElement } from "react";
import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { CodeComposer } from "./CodeComposer";
import { useCodeUiStore } from "./CodeUiStore";
import { HttpError } from "../api/client";
import { useComposerDrafts } from "../ComposerDrafts";
import { useUiStore } from "../UiStore";

function app(): AppContextValue {
  return {
    client: {} as never,
    models: [],
    defaultModelKey: null,
    providers: [],
    refreshCatalog: async () => {},
    refreshChats: async () => {},
    status: "",
    setStatus: () => {},
    newChat: () => {},
    deleteChat: () => {},
    startRename: () => {},
    commitRename: () => {},
    cancelRename: () => {},
    newProject: async () => false,
    deleteProject: () => {},
    startProjectRename: () => {},
    commitProjectRename: () => {},
    cancelProjectRename: () => {},
    newChatInProject: () => {},
    moveChatToProject: () => {},
    updateState: { status: "idle", version: null, error: null, enabled: false },
    updateUpToDate: false,
    checkForUpdate: async () => ({
      status: "idle",
      version: null,
      error: null,
      enabled: false,
    }),
    restartForUpdate: async () => {},
  };
}

function renderComposer(ui: ReactElement) {
  return render(<AppContextProvider value={app()}>{ui}</AppContextProvider>);
}

beforeEach(() => {
  URL.createObjectURL = vi.fn((): string => "blob:preview");
  URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  cleanup();
  useUiStore.setState({ activeTurnSendMode: "queue" });
  useCodeUiStore.setState({ pendingComposerPrompt: null });
  useComposerDrafts.setState({ attachments: {} });
});

const QUEUED = {
  kind: "queued" as const,
  queued: { session_id: "sess-1", message: "and run the tests", position: 1 },
};

describe("CodeComposer", () => {
  it("inserts a pending inspector prompt into the draft", async () => {
    useCodeUiStore.getState().offerComposerPrompt("Merge pull request #41.");
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    expect(await screen.findByRole("textbox", { name: "Message" })).toHaveValue(
      "Merge pull request #41.",
    );
    expect(useCodeUiStore.getState().pendingComposerPrompt).toBeNull();
  });

  it("states the session's mode in the composer", () => {
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "Permissions: Ask" })).toBeInTheDocument();
  });

  it("shows the chat context meter from the last turn's usage", () => {
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        contextUsage={{
          usage: {
            input_tokens: 11_000,
            output_tokens: 12,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
          },
          contextWindow: 200_000,
          modelName: "Sonnet",
        }}
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    expect(
      screen.getByRole("button", { name: "Context: 6% of 200k tokens used" }),
    ).toBeInTheDocument();
  });

  it("explains why the session permission mode cannot change", async () => {
    const user = userEvent.setup();
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    const trigger = screen.getByRole("button", { name: "Permissions: Ask" });
    expect(trigger).toBeDisabled();
    await user.hover(trigger.parentElement!);
    expect(await screen.findByRole("tooltip")).toHaveTextContent(
      "Set when the session started — start a new session to change it",
    );
  });

  it("queues a follow-up written while a turn is running", async () => {
    const onSend = vi.fn().mockResolvedValue(QUEUED);
    renderComposer(
      <CodeComposer
        running
        permissionMode="ask"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "Stop response" })).toBeEnabled();

    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "and run the tests" } });
    expect(screen.getByRole("button", { name: "Queue message for after this response" })).toBeEnabled();
    fireEvent.keyDown(box, { key: "Enter" });

    await waitFor(() => expect(onSend).toHaveBeenCalledWith("and run the tests"));
    expect(box).toHaveValue("");
    expect(await screen.findByText("1 follow-up queued")).toBeInTheDocument();
  });

  it("clears the queued pill when the next turn begins", async () => {
    const onSend = vi.fn().mockResolvedValue(QUEUED);
    const { rerender } = renderComposer(
      <CodeComposer
        running
        permissionMode="ask"
        lastTurnBeganId="t1"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "and run the tests" } });
    fireEvent.keyDown(box, { key: "Enter" });
    expect(await screen.findByText("1 follow-up queued")).toBeInTheDocument();

    rerender(
      <AppContextProvider value={app()}>
        <CodeComposer
          running
          permissionMode="ask"
          lastTurnBeganId="t2"
          onSend={onSend}
          onInterrupt={vi.fn()}
        />
      </AppContextProvider>,
    );
    expect(screen.queryByText("1 follow-up queued")).toBeNull();
  });

  it("steers mid-turn when the harness supports it", async () => {
    useUiStore.setState({ activeTurnSendMode: "steer" });
    const onSteer = vi.fn().mockResolvedValue(undefined);
    const onSend = vi.fn();
    renderComposer(
      <CodeComposer
        running
        permissionMode="ask"
        onSend={onSend}
        onSteer={onSteer}
        onInterrupt={vi.fn()}
      />,
    );

    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "try the other file" } });
    fireEvent.keyDown(box, { key: "Enter" });

    await waitFor(() =>
      expect(onSteer).toHaveBeenCalledWith("try the other file"),
    );
    expect(onSend).not.toHaveBeenCalled();
    expect(await screen.findByText("Guidance sent")).toBeInTheDocument();
  });

  it("queues mid-turn when steering is unsupported", async () => {
    useUiStore.setState({ activeTurnSendMode: "steer" });
    const onSend = vi.fn().mockResolvedValue(QUEUED);
    renderComposer(
      <CodeComposer
        running
        permissionMode="ask"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "and run the tests" } });
    fireEvent.keyDown(box, { key: "Enter" });

    await waitFor(() => expect(onSend).toHaveBeenCalledWith("and run the tests"));
    expect(await screen.findByText("1 follow-up queued")).toBeInTheDocument();
  });

  it("submits free-typed slash text verbatim when the engine lists no commands", async () => {
    const onSend = vi.fn().mockResolvedValue(undefined);
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "/status --json" } });
    expect(screen.queryByRole("listbox")).toBeNull();
    fireEvent.keyDown(box, { key: "Enter" });

    await waitFor(() => expect(onSend).toHaveBeenCalledWith("/status --json"));
  });

  it("says the queue is full and keeps the draft", async () => {
    const onSend = vi
      .fn()
      .mockRejectedValue(
        new HttpError(409, "409: a follow-up is already queued", "queue_full"),
      );
    renderComposer(
      <CodeComposer
        running
        permissionMode="ask"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "and push it" } });
    fireEvent.click(
      screen.getByRole("button", { name: "Queue message for after this response" }),
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "A follow-up is already queued. Wait for it to run, or interrupt this turn.",
    );
    expect(box).toHaveValue("and push it");
  });

  it("keeps the draft when send is refused", async () => {
    const onSend = vi.fn().mockRejectedValue(new Error("session is fenced"));
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="plan"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );
    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "list the files" } });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(onSend).toHaveBeenCalled());
    expect(box).toHaveValue("list the files");
  });

  it("changes the selected harness model", async () => {
    const user = userEvent.setup();
    const onModelChange = vi.fn();
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        harness="claude_code"
        model="sonnet"
        modelOptions={[
          {
            id: "sonnet",
            label: "Sonnet 4.6",
            source: "Claude Code",
            default: true,
          },
          { id: "opus", label: "Opus 4.6", source: "Claude Code" },
        ]}
        onModelChange={onModelChange}
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Model: Sonnet 4.6" }));
    await user.click(screen.getByRole("menuitem", { name: /Opus 4.6/ }));
    expect(onModelChange).toHaveBeenCalledWith("opus");
  });

  it("shows reasoning effort next to a model that accepts levels", () => {
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        harness="codex"
        model="gpt-5.4"
        modelOptions={[
          {
            id: "gpt-5.4",
            label: "GPT-5.4",
            source: "Codex CLI",
            reasoning_efforts: ["low", "medium", "high"],
          },
        ]}
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    const model = screen.getByRole("button", { name: "Model: GPT-5.4" });
    const effort = screen.getByRole("button", { name: "Reasoning: Default" });
    expect(
      model.compareDocumentPosition(effort) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("hides reasoning effort when the model accepts none", () => {
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        harness="claude_code"
        model="sonnet"
        modelOptions={[
          {
            id: "sonnet",
            label: "Sonnet 4.6",
            source: "Claude Code",
            reasoning_efforts: [],
          },
        ]}
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "Model: Sonnet 4.6" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Reasoning:/ })).toBeNull();
  });

  it("claims an image paste and leaves a text paste alone", () => {
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        sessionId="sess-1"
        imageInput
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    const box = screen.getByRole("textbox", { name: "Message" });
    const image = pasteOn(box, [
      new File([new Uint8Array([1, 2, 3, 4])], "shot.png", {
        type: "image/png",
      }),
    ]);
    expect(image.defaultPrevented).toBe(true);
    expect(screen.getByLabelText("Attached images")).toBeInTheDocument();
    expect(screen.getByText("shot.png")).toBeInTheDocument();

    const text = pasteOn(box, []);
    expect(text.defaultPrevented).toBe(false);
  });

  it("opens the image picker from the tools menu", async () => {
    const user = userEvent.setup();
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        sessionId="sess-1"
        imageInput
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    const input = document.querySelector<HTMLInputElement>(
      'input[type="file"][accept]',
    );
    expect(input).not.toBeNull();
    const click = vi.spyOn(input!, "click");
    await user.click(screen.getByRole("button", { name: "Tools" }));
    await user.click(screen.getByRole("menuitem", { name: "Attach files" }));
    expect(click).toHaveBeenCalled();
  });

  it("does not send while an attached image has not published", async () => {
    const onSend = vi.fn();
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        sessionId="sess-1"
        imageInput
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    const box = screen.getByRole("textbox", { name: "Message" });
    pasteOn(box, [
      new File([new Uint8Array([1, 2, 3, 4])], "shot.png", {
        type: "image/png",
      }),
    ]);
    fireEvent.change(box, { target: { value: "what is in this" } });
    expect(await screen.findAllByText("shot.png")).not.toHaveLength(0);
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    expect(onSend).not.toHaveBeenCalled();
  });
});

function pasteOn(target: Element, files: File[]) {
  const event = createEvent.paste(target, {
    clipboardData: { files },
  });
  fireEvent(target, event);
  return event;
}
