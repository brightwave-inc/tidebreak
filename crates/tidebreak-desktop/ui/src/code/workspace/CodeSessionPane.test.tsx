// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApiClient } from "@/api/client";
import type {
  CodeApprovalSnapshot,
  CodeApprovalDecision,
  CodeSessionSnapshot,
} from "@/api/types";
import { codeSession } from "@/stories/fixtures";
import { useCodeCatalogStore } from "../CodeCatalogStore";
import { resetCodeSessionRegistry } from "../CodeSessionRegistry";
import { CodeSessionPane } from "./CodeSessionPane";

const navigate = vi.hoisted(() => vi.fn());
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => navigate,
}));

const composer = vi.hoisted(() => ({
  props: null as null | {
    model?: string;
    onSend: (message: string) => Promise<unknown>;
    onEffortChange?: (effort: "high") => void;
  },
}));
vi.mock("../CodeComposer", () => ({
  CodeComposer: (props: typeof composer.props) => {
    composer.props = props;
    return null;
  },
}));
const transcript = vi.hoisted(() => ({
  props: null as null | {
    onDecide?: (
      approvalId: string,
      decision: CodeApprovalDecision,
      feedback?: string,
    ) => Promise<void>;
    approvalError?: string;
    approvalErrorId?: string | null;
    decidingId?: string | null;
  },
}));
vi.mock("../CodeTranscript", () => ({
  CodeTranscript: (props: typeof transcript.props) => {
    transcript.props = props;
    return null;
  },
}));
vi.mock("@/QueueTray", () => ({
  QueueTray: () => null,
  useCodeQueueApi: () => ({}),
}));
vi.mock("@/useTranscriptFollow", () => ({
  useTranscriptFollow: () => ({
    armFollow: vi.fn(),
    requestSmoothFollow: vi.fn(),
    pauseFollow: vi.fn(),
    scrollRef: { current: null },
    contentRef: { current: null },
    onScroll: vi.fn(),
  }),
}));

function setup(session: CodeSessionSnapshot) {
  const submitCodeTurn = vi.fn(async () => ({ kind: "queued" }));
  const decideCodeApproval = vi.fn(async () => ({}) as CodeApprovalSnapshot);
  const client = {
    submitCodeTurn,
    decideCodeApproval,
    listCodeSessionTurns: async () => [],
    listCodeApprovals: async () => [],
    openCodeEvents: () => ({
      close() {},
      addEventListener() {},
      removeEventListener() {},
    }),
  } as unknown as ApiClient;
  const props = {
    session,
    client,
    catalogModels: [],
    defaultModelKey: null,
    disabled: false,
  };
  return {
    submitCodeTurn,
    decideCodeApproval,
    props,
    ...render(<CodeSessionPane {...props} />),
  };
}
beforeEach(() => {
  navigate.mockReset();
  composer.props = null;
  transcript.props = null;
  useCodeCatalogStore.getState().reset();
  useCodeCatalogStore.getState().rememberHarnessModels("claude_code", [
    {
      id: "inferred-default",
      label: "Default",
      source: "Harness",
      default: true,
    },
  ]);
});
afterEach(() => {
  cleanup();
  resetCodeSessionRegistry();
});
it.each([undefined, "stale-owner-model"])(
  "omits inferred or stale model overrides from contributor replies (%s)",
  async (model) => {
    const { submitCodeTurn } = setup({
      ...codeSession,
      lifecycle: "idle",
      model,
      is_owner: false,
      access: "contribute",
    });
    expect(composer.props?.model).toBe(model ?? "inferred-default");
    await act(async () => {
      await composer.props?.onSend("continue");
    });
    expect(submitCodeTurn).toHaveBeenCalledWith(
      codeSession.id,
      "continue",
      undefined,
      undefined,
    );
  },
);
it("drops pending reasoning overrides after ownership is lost", async () => {
  const session = {
    ...codeSession,
    is_owner: true,
    access: "contribute" as const,
  };
  const { props, rerender, submitCodeTurn } = setup(session);
  expect(composer.props?.onEffortChange).toBeTypeOf("function");
  await act(async () => {
    composer.props?.onEffortChange?.("high");
  });
  rerender(
    <CodeSessionPane {...props} session={{ ...session, is_owner: false }} />,
  );
  await act(async () => {
    await composer.props?.onSend("continue");
  });
  expect(submitCodeTurn).toHaveBeenCalledWith(
    codeSession.id,
    "continue",
    undefined,
    undefined,
  );
});
it("keeps a deliberate owner model override", async () => {
  const { submitCodeTurn } = setup({
    ...codeSession,
    lifecycle: "idle",
    model: "owner-model",
    is_owner: true,
    access: "contribute",
  });
  await act(async () => {
    await composer.props?.onSend("continue");
  });
  expect(submitCodeTurn).toHaveBeenCalledWith(
    codeSession.id,
    "continue",
    "owner-model",
    undefined,
  );
});

it("passes structured answers to the client and keeps failure attached after sending ends", async () => {
  const { decideCodeApproval } = setup({
    ...codeSession,
    lifecycle: "idle",
    is_owner: true,
  });
  decideCodeApproval.mockRejectedValue(new Error("Answer could not be saved."));
  const decision: CodeApprovalDecision = {
    answers: { answers: [{ question_id: "q1", selected_option_ids: ["one"] }] },
  };
  await act(async () => {
    await transcript.props?.onDecide?.("approval-one", decision);
  });
  expect(decideCodeApproval).toHaveBeenCalledWith("approval-one", {
    decision,
    feedback: undefined,
  });
  expect(transcript.props?.decidingId).toBeNull();
  expect(transcript.props?.approvalErrorId).toBe("approval-one");
  expect(transcript.props?.approvalError).toBeTruthy();
});

it("opens a child session from the parent tree in one click", async () => {
  setup({
    ...codeSession,
    children: [
      {
        id: "child-session",
        title: "Inspect the parser",
        status: "fenced",
        attention: true,
        fenced: true,
        workspace_id: "ws-child",
        execution_location: "sandbox",
      },
    ],
    wait: { waiting: 1, total: 1 },
  });
  expect(screen.getByText("Waiting on 1 of 1")).toBeTruthy();
  expect(screen.getByText("Needs attention").parentElement).toHaveTextContent(
    "Needs attention · Sandbox",
  );
  expect(screen.queryByText("fenced")).toBeNull();
  await act(async () => {
    screen.getByRole("button", { name: "Open Inspect the parser" }).click();
  });
  expect(navigate).toHaveBeenCalledWith({
    to: "/code/w/$workspaceId",
    params: { workspaceId: "ws-child" },
    search: { task: "child-session" },
  });
});
