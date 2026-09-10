// @vitest-environment jsdom
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApiClient } from "@/api/client";
import type { CodeSessionSnapshot } from "@/api/types";
import { codeSession } from "@/stories/fixtures";
import { useCodeCatalogStore } from "../CodeCatalogStore";
import { resetCodeSessionRegistry } from "../CodeSessionRegistry";
import { CodeSessionPane } from "./CodeSessionPane";

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
vi.mock("../CodeTranscript", () => ({ CodeTranscript: () => null }));
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
  const client = {
    submitCodeTurn,
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
  return { submitCodeTurn, props, ...render(<CodeSessionPane {...props} />) };
}
beforeEach(() => {
  composer.props = null;
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
