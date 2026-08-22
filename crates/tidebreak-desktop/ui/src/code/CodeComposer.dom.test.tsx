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
import { renderToStaticMarkup } from "react-dom/server";
import { AppContextProvider, type AppContextValue } from "@/AppContext";
import { CodeComposer, HarnessModelMenu } from "./CodeComposer";
import { useCodeUiStore } from "./CodeUiStore";
import { HttpError } from "../api/client";
import { useComposerDrafts } from "../ComposerDrafts";
import { readyImageAttachment } from "../ImageAttachments";
import { OpenAIIcon, ProviderIcon } from "../ProviderIcons";
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
  useCodeUiStore.setState({
    pendingComposerPrompt: null,
    composerActionScope: null,
  });
  useComposerDrafts.setState({ attachments: {} });
});

const QUEUED = {
  kind: "queued" as const,
  queued: { session_id: "sess-1", message: "and run the tests", position: 1 },
};

describe("CodeComposer", () => {
  it("inserts a pending inspector prompt into the draft", async () => {
    useCodeUiStore
      .getState()
      .offerComposerPrompt("code", "Merge pull request #41.");
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

  it("submits a one-click workspace action without replacing the draft", async () => {
    const onSend = vi.fn();
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );
    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "Keep my draft" } });

    useCodeUiStore
      .getState()
      .runComposerPrompt("code", "Fix CI for pull request #41.");

    await waitFor(() =>
      expect(onSend).toHaveBeenCalledWith("Fix CI for pull request #41."),
    );
    expect(box).toHaveValue("Keep my draft");
    expect(useCodeUiStore.getState().pendingComposerPrompt).toBeNull();
  });

  it("keeps a one-click action in the draft when the composer cannot run", async () => {
    const onSend = vi.fn();
    useCodeUiStore
      .getState()
      .runComposerPrompt("code", "Resolve conflicts for pull request #41.");
    renderComposer(
      <CodeComposer
        disabled
        running={false}
        permissionMode="ask"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    expect(await screen.findByRole("textbox", { name: "Message" })).toHaveValue(
      "Resolve conflicts for pull request #41.",
    );
    expect(onSend).not.toHaveBeenCalled();
  });

  it("does not consume an action intended for another workspace", async () => {
    const onSend = vi.fn();
    useCodeUiStore
      .getState()
      .runComposerPrompt("workspace-a", "Merge pull request #41.");
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        promptScope="workspace-b"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    expect(screen.getByRole("textbox", { name: "Message" })).toHaveValue("");
    expect(onSend).not.toHaveBeenCalled();
    expect(useCodeUiStore.getState().pendingComposerPrompt?.scope).toBe(
      "workspace-a",
    );
  });

  it("locks repeated one-click actions until the first submission settles", async () => {
    let release!: () => void;
    const pending = new Promise<void>((resolve) => {
      release = resolve;
    });
    const onSend = vi.fn(() => pending);
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        promptScope="workspace-a"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    expect(
      useCodeUiStore
        .getState()
        .runComposerPrompt("workspace-a", "Fix CI for pull request #41."),
    ).toBe(true);
    expect(
      useCodeUiStore
        .getState()
        .runComposerPrompt("workspace-a", "Fix CI for pull request #41."),
    ).toBe(false);
    await waitFor(() => expect(onSend).toHaveBeenCalledTimes(1));
    expect(useCodeUiStore.getState().composerActionScope).toBe("workspace-a");

    release();
    await waitFor(() =>
      expect(useCodeUiStore.getState().composerActionScope).toBeNull(),
    );
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

    expect(
      screen.getByRole("button", { name: "Permissions: Ask" }),
    ).toBeInTheDocument();
  });

  it("shows the chat context meter from the last turn's usage", () => {
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        contextUsage={{
          contextTokens: 11_000,
          spend: {
            input: 11_000,
            output: 12,
            cacheRead: 0,
            cacheWrite: 0,
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

  it("explains why the permission mode cannot change without a handler", async () => {
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

  /**
   * A live session can re-posture: the engine takes the new mode on its own
   * channel where it has one, and is relaunched where it does not.
   */
  it("changes the permission mode of a live session", async () => {
    const user = userEvent.setup();
    const onModeChange = vi.fn();
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        availableModes={["plan", "ask", "auto"]}
        sessionId="sess-1"
        onModeChange={onModeChange}
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    const trigger = screen.getByRole("button", { name: "Permissions: Ask" });
    expect(trigger).toBeEnabled();
    await user.click(trigger);
    await user.click(screen.getByRole("menuitem", { name: /Auto/ }));
    expect(onModeChange).toHaveBeenCalledWith("auto");
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
    expect(
      screen.getByRole("button", {
        name: "Queue message for after this response",
      }),
    ).toBeEnabled();
    fireEvent.keyDown(box, { key: "Enter" });

    await waitFor(() =>
      expect(onSend).toHaveBeenCalledWith("and run the tests"),
    );
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

  it("preserves text typed while a steer request is pending", async () => {
    useUiStore.setState({ activeTurnSendMode: "steer" });
    let resolveSteer!: () => void;
    const onSteer = vi.fn(
      () =>
        new Promise<void>((resolve) => {
          resolveSteer = resolve;
        }),
    );
    renderComposer(
      <CodeComposer
        running
        permissionMode="ask"
        onSend={vi.fn()}
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

    fireEvent.change(box, { target: { value: "also keep the API small" } });
    resolveSteer();

    await waitFor(() =>
      expect(screen.getByText("Guidance sent")).toBeInTheDocument(),
    );
    expect(box).toHaveValue("also keep the API small");
  });

  it("refuses unsupported steering without silently queueing or clearing the draft", async () => {
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

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Redirect isn’t available for this harness. Choose Queue to send this after the response.",
    );
    expect(onSend).not.toHaveBeenCalled();
    expect(box).toHaveValue("and run the tests");
    expect(screen.queryByText("1 follow-up queued")).toBeNull();
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
      screen.getByRole("button", {
        name: "Queue message for after this response",
      }),
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
        onEffortChange={vi.fn()}
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

  /**
   * The session's own level labels the button, and picking one reports it. A
   * control that only moved locally would claim a level the next turn does
   * not run at.
   */
  it("labels reasoning effort from the session and reports a change", async () => {
    const user = userEvent.setup();
    const onEffortChange = vi.fn();
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        harness="codex"
        model="gpt-5.4"
        reasoningEffort="high"
        modelOptions={[
          {
            id: "gpt-5.4",
            label: "GPT-5.4",
            source: "Codex CLI",
            reasoning_efforts: ["low", "medium", "high", "xhigh", "ultra"],
          },
        ]}
        onEffortChange={onEffortChange}
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Reasoning: High" }));
    await user.click(screen.getByRole("menuitem", { name: /Ultra/ }));
    expect(onEffortChange).toHaveBeenCalledWith("ultra");
    expect(
      screen.getByRole("button", { name: "Reasoning: Ultra" }),
    ).toHaveAttribute("data-ultra", "on");
  });

  /**
   * The treatment marks the top of whatever ladder is on offer, so an engine
   * that stops at `xhigh` gets it there and nothing hard-codes `ultra`.
   */
  it("marks the top rung of a shorter ladder", () => {
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        harness="grok"
        model="grok-4.6"
        reasoningEffort="xhigh"
        modelOptions={[
          {
            id: "grok-4.6",
            label: "Grok 4.6",
            source: "Grok CLI",
            reasoning_efforts: ["low", "medium", "high", "xhigh"],
          },
        ]}
        onEffortChange={vi.fn()}
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    expect(
      screen.getByRole("button", { name: "Reasoning: X-high" }),
    ).toHaveAttribute("data-ultra", "on");
  });

  /**
   * No handler means nothing would persist the choice, so the control stays
   * hidden rather than offering a level the next turn would not run at.
   */
  it("hides reasoning effort when the caller cannot persist it", () => {
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

    expect(screen.queryByRole("button", { name: /Reasoning:/ })).toBeNull();
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
        onEffortChange={vi.fn()}
        onSend={vi.fn()}
        onInterrupt={vi.fn()}
      />,
    );

    expect(
      screen.getByRole("button", { name: "Model: Sonnet 4.6" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Reasoning:/ })).toBeNull();
  });

  it("claims an image paste and leaves a text paste alone", () => {
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        sessionId="sess-1"
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

  it("attaches an image on an engine with no protocol for one", () => {
    // The bytes reach the engine as a file under the checkout, so the
    // composer offers attachment whatever the engine's own input path is.
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        harness="opencode"
        sessionId="sess-1"
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
  });

  it("opens the image picker from the tools menu", async () => {
    const user = userEvent.setup();
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        sessionId="sess-1"
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

  it("drops attached images as soon as send starts", async () => {
    const onSend = vi.fn(() => new Promise<void>(() => {}));
    useComposerDrafts.getState().setImages("sess-1", [
      readyImageAttachment("img-1", {
        attachmentId: "1c2f1a44-2f3b-4a1e-9f0a-2b6d5c4e3a21",
        fileName: "shot.png",
        mediaType: "image/png",
        width: 390,
        height: 202,
        byteLen: 1024,
      }),
    ]);
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        sessionId="sess-1"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    expect(screen.getByText("shot.png")).toBeInTheDocument();
    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "look at this" } });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    await waitFor(() => expect(onSend).toHaveBeenCalled());
    expect(screen.queryByLabelText("Attached images")).toBeNull();
  });

  it("puts images back when send is refused", async () => {
    const onSend = vi
      .fn()
      .mockRejectedValue(new Error("Could not send that turn"));
    useComposerDrafts.getState().setImages("sess-1", [
      readyImageAttachment("img-1", {
        attachmentId: "1c2f1a44-2f3b-4a1e-9f0a-2b6d5c4e3a21",
        fileName: "shot.png",
        mediaType: "image/png",
        width: 390,
        height: 202,
        byteLen: 1024,
      }),
    ]);
    renderComposer(
      <CodeComposer
        running={false}
        permissionMode="ask"
        sessionId="sess-1"
        onSend={onSend}
        onInterrupt={vi.fn()}
      />,
    );

    const box = screen.getByRole("textbox", { name: "Message" });
    fireEvent.change(box, { target: { value: "look at this" } });
    fireEvent.click(screen.getByRole("button", { name: "Send message" }));
    expect(await screen.findByText("shot.png")).toBeInTheDocument();
    expect(box).toHaveValue("look at this");
  });
});

describe("HarnessModelMenu", () => {
  it("brands an open-model row by its family, not the engine", () => {
    const option = {
      id: "deepseek-v4-flash-0731",
      label: "DeepSeek V4 Flash 0731",
      source: "Codex CLI",
    };
    const markup = renderToStaticMarkup(
      <HarnessModelMenu
        harness="codex"
        options={[option]}
        value={option.id}
        onChange={() => {}}
        variant="field"
      />,
    );
    expect(markup).toContain(
      renderToStaticMarkup(
        <ProviderIcon
          provider="model_gateway"
          modelId={option.id}
          className="size-4 shrink-0"
        />,
      ),
    );
    expect(markup).not.toContain(
      renderToStaticMarkup(<OpenAIIcon className="size-4 shrink-0" />),
    );
  });
});

function pasteOn(target: Element, files: File[]) {
  const event = createEvent.paste(target, {
    clipboardData: { files },
  });
  fireEvent(target, event);
  return event;
}
