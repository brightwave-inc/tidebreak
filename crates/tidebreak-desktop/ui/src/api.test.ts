import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiClient,
  archiveForceKind,
  HttpError,
  parseAgentActivityHistory,
  parseAgentRunTaskPlan,
  parseFolderAccessRequest,
  parseInboxItem,
  parsePendingChatPrompt,
  parseOutputWritebackRequest,
  parsePendingUserQuestions,
  parsePendingToolApproval,
  parseSandboxAgentCancellation,
  parseTaskPlan,
  parseToolActionPreview,
  parseToolResultPreview,
} from "./api";

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("parseFolderAccessRequest", () => {
  it("accepts only the closed renderer-safe consent projection", () => {
    expect(
      parseFolderAccessRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        reason:
          "The assistant needs read access to files outside the folders connected to this conversation.",
        folder_hint: "documents",
        claimed: true,
      }),
    ).toEqual({
      callId: "call-1",
      turnId: "turn-1",
      reason:
        "The assistant needs read access to files outside the folders connected to this conversation.",
      folderHint: "documents",
      claimedByDesktop: true,
    });

    expect(
      parseFolderAccessRequest({
        call_id: "call-2",
        turn_id: "turn-2",
        reason:
          "The assistant needs read access to files outside the folders connected to this conversation.",
        folder_hint: null,
        claimed: false,
      }),
    ).toEqual({
      callId: "call-2",
      turnId: "turn-2",
      reason:
        "The assistant needs read access to files outside the folders connected to this conversation.",
      folderHint: null,
      claimedByDesktop: false,
    });
  });

  it("rejects canonical tool records and extended projections", () => {
    expect(
      parseFolderAccessRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        reason:
          "The assistant needs read access to files outside the folders connected to this conversation.",
        folder_hint: null,
        claimed: false,
        arguments: { path: "/Users/private" },
      }),
    ).toBeNull();

    expect(
      parseFolderAccessRequest({
        id: "call-1",
        chat_id: "chat-1",
        turn_id: "turn-1",
        name: "request_folder_access",
        arguments: { reason: "Read files" },
        provider_id: "provider-secret",
        client_executor_id: "executor-secret",
      }),
    ).toBeNull();
  });

  it("rejects malformed user-facing fields", () => {
    expect(
      parseFolderAccessRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        reason: "Model-controlled secret prose",
        folder_hint: null,
        claimed: false,
      }),
    ).toBeNull();

    expect(
      parseFolderAccessRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        reason: " ",
        folder_hint: "desktop",
        claimed: "yes",
      }),
    ).toBeNull();
  });
});

describe("inbox items", () => {
  const safe = {
    chat_id: "chat-1",
    chat_title: "Quarterly review",
    turn_id: "turn-1",
    call_id: "call-1",
    kind: "tool_approval",
    action: "exec",
    requested_at: "2026-08-04T00:00:00Z",
  };

  it("accepts the read model, with its optional fields absent", () => {
    expect(parseInboxItem(safe)).toEqual({
      chatId: "chat-1",
      chatTitle: "Quarterly review",
      turnId: "turn-1",
      callId: "call-1",
      kind: "tool_approval",
      action: "exec",
      requestedAt: "2026-08-04T00:00:00Z",
    });
    const { chat_title: _title, action: _action, ...untitled } = safe;
    expect(parseInboxItem({ ...untitled, kind: "question" })).toMatchObject({
      chatTitle: null,
      action: null,
      kind: "question",
    });
  });

  it("rejects an unknown kind, an unknown tool, and smuggled detail", () => {
    expect(parseInboxItem({ ...safe, kind: "everything" })).toBeNull();
    expect(parseInboxItem({ ...safe, action: "rm_rf" })).toBeNull();
    expect(
      parseInboxItem({ ...safe, questions: [{ question: "private" }] }),
    ).toBeNull();
  });
});

describe("pending chat prompt summaries", () => {
  const safe = {
    chat_id: "chat-1",
    question_call_ids: ["question-1"],
    plan_call_ids: ["plan-1"],
    folder_access_call_ids: ["folder-1"],
    output_writeback_call_ids: ["writeback-1"],
  };

  it("accepts only opaque prompt identities", () => {
    expect(parsePendingChatPrompt(safe)).toEqual({
      chatId: "chat-1",
      questionCallIds: ["question-1"],
      planCallIds: ["plan-1"],
      folderAccessCallIds: ["folder-1"],
      outputWritebackCallIds: ["writeback-1"],
    });
  });

  it("rejects detail, duplicate identities, and empty summaries", () => {
    expect(parsePendingChatPrompt({ ...safe, questions: [{ question: "private" }] })).toBeNull();
    expect(
      parsePendingChatPrompt({ ...safe, folder_access_call_ids: ["question-1"] }),
    ).toBeNull();
    expect(
      parsePendingChatPrompt({
        ...safe,
        question_call_ids: [],
        plan_call_ids: [],
        folder_access_call_ids: [],
        output_writeback_call_ids: [],
      }),
    ).toBeNull();
  });

  it("fetches and validates the complete page", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify([safe]), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await expect(client.listPendingChatPrompts()).resolves.toEqual([
      {
        chatId: "chat-1",
        questionCallIds: ["question-1"],
        planCallIds: ["plan-1"],
        folderAccessCallIds: ["folder-1"],
        outputWritebackCallIds: ["writeback-1"],
      },
    ]);
    expect(fetchMock.mock.calls[0][0]).toBe("http://127.0.0.1/chats/pending-prompts");
  });
});

describe("parseOutputWritebackRequest", () => {
  it("accepts only opaque identities, mode, and claimed state", () => {
    expect(
      parseOutputWritebackRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        mode: "create",
        claimed: false,
      }),
    ).toEqual({
      callId: "call-1",
      turnId: "turn-1",
      mode: "create",
      claimedByDesktop: false,
    });
    expect(
      parseOutputWritebackRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        mode: "append",
        claimed: false,
      }),
    ).toBeNull();
    expect(
      parseOutputWritebackRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        mode: "replace",
        claimed: false,
        path: "private/file.txt",
      }),
    ).toBeNull();
  });
});

describe("parseTaskPlan", () => {
  const safe = {
    turn_id: "turn-1",
    updated_at: "2026-08-06T12:00:00Z",
    steps: [
      { content: "Read the spec", status: "completed" },
      { content: "Draft the change", status: "in_progress" },
    ],
  };

  it("accepts a well-formed plan in the order the agent wrote it", () => {
    expect(parseTaskPlan(safe)).toEqual(safe);
  });

  // All or nothing: a plan is written as one replacement, so dropping the step
  // that failed validation would leave a checklist that silently disagrees
  // with the work the agent thinks it is doing.
  it.each([
    ["an unknown key", { ...safe, note: "extra" }],
    [
      "a status outside the closed vocabulary",
      { ...safe, steps: [{ content: "Read the spec", status: "skipped" }] },
    ],
    [
      "a step past the server's own length limit",
      { ...safe, steps: [{ content: "x".repeat(501), status: "pending" }] },
    ],
    [
      "a step carrying a line break",
      { ...safe, steps: [{ content: "one\ntwo", status: "pending" }] },
    ],
    ["no steps at all", { ...safe, steps: [] }],
  ])("rejects the whole plan over %s", (_case, payload) => {
    expect(parseTaskPlan(payload)).toBeNull();
  });
});

describe("parseAgentRunTaskPlan", () => {
  const safe = {
    run_id: "run-1",
    updated_at: "2026-08-07T12:00:00Z",
    steps: [
      { content: "Gather the figures", status: "completed" },
      { content: "Write the summary", status: "in_progress" },
    ],
  };

  it("accepts a well-formed run plan", () => {
    expect(parseAgentRunTaskPlan(safe)).toEqual(safe);
  });

  // The steps go through the same validation the chat plan's do, so this
  // covers the run-shaped envelope and one representative step failure rather
  // than restating the step table already pinned above.
  it.each([
    ["a plan keyed by turn rather than run", { ...safe, turn_id: "turn-1" }],
    [
      "a step outside the closed status vocabulary",
      { ...safe, steps: [{ content: "Write it", status: "skipped" }] },
    ],
    ["no steps at all", { ...safe, steps: [] }],
  ])("rejects the whole plan over %s", (_case, payload) => {
    expect(parseAgentRunTaskPlan(payload)).toBeNull();
  });
});

describe("parsePendingUserQuestions", () => {
  const safe = {
    call_id: "call-1",
    turn_id: "turn-1",
    asked_at: "2026-07-24T12:00:00Z",
    questions: [
      {
        id: "target",
        header: "Target",
        question: "Where should I deploy?",
        options: [
          {
            id: "staging",
            label: "Staging",
            description: "Deploy for internal verification.",
          },
        ],
        question_type: "multi_select",
        allow_free_form: true,
      },
    ],
  };

  it("accepts the closed bounded projection", () => {
    expect(parsePendingUserQuestions(safe)).toEqual({
      callId: "call-1",
      turnId: "turn-1",
      askedAt: "2026-07-24T12:00:00Z",
      questions: [
        {
          id: "target",
          header: "Target",
          question: "Where should I deploy?",
          options: [
            {
              id: "staging",
              label: "Staging",
              description: "Deploy for internal verification.",
            },
          ],
          questionType: "multi_select",
          allowFreeForm: true,
        },
      ],
    });
  });

  it("rejects private, duplicate, and unanswerable payloads", () => {
    expect(
      parsePendingUserQuestions({ ...safe, provider_id: "private" }),
    ).toBeNull();
    expect(
      parsePendingUserQuestions({
        ...safe,
        questions: [safe.questions[0], safe.questions[0]],
      }),
    ).toBeNull();
    expect(
      parsePendingUserQuestions({
        ...safe,
        questions: [
          { ...safe.questions[0], options: [], allow_free_form: false },
        ],
      }),
    ).toBeNull();
    expect(
      parsePendingUserQuestions({
        ...safe,
        questions: [{ ...safe.questions[0], header: "Target\u0085hidden" }],
      }),
    ).toBeNull();
    expect(
      parsePendingUserQuestions({
        ...safe,
        questions: [{ ...safe.questions[0], question_type: "ranked" }],
      }),
    ).toBeNull();
  });
});

describe("pending approval recovery", () => {
  const safe = {
    call_id: "call-1",
    turn_id: "turn-1",
    action: "search",
    approval: "search_may_share_query_and_excerpts",
    class: "sensitive",
    can_approve: true,
    can_remember: true,
    grant_rungs: ["whole_tool"],
  };

  it("parses only the closed renderer projection", () => {
    expect(parsePendingToolApproval(safe)).toEqual({
      callId: "call-1",
      turnId: "turn-1",
      action: "search",
      approval: "search_may_share_query_and_excerpts",
      class: "sensitive",
      preview: null,
      canApprove: true,
      canRemember: true,
      grantRungs: ["whole_tool"],
      autoJudgeStatus: null,
    });
    expect(parsePendingToolApproval({ ...safe, arguments: { query: "private" } })).toBeNull();
    expect(parsePendingToolApproval({ ...safe, can_approve: false })).toBeNull();
    expect(parsePendingToolApproval({ ...safe, action: "private_plugin" })).toBeNull();
    expect(parsePendingToolApproval({ ...safe, grant_rungs: [] })).toBeNull();
    expect(
      parsePendingToolApproval({ ...safe, grant_rungs: ["unknown_scope"] }),
    ).toBeNull();
  });

  it("recovers the command an exec approval is granting", () => {
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "exec",
        approval: "exec_may_run_networked_command",
        preview: {
          tool: "exec",
          command: "cargo",
          args: ["test", "--workspace"],
          cwd: "checkout",
          files: [],
        },
      })?.preview,
    ).toEqual({
      tool: "exec",
      command: "cargo",
      args: ["test", "--workspace"],
      cwd: "checkout",
      files: [],
    });
  });

  // A pending write_file approval offers path_prefix rungs. Rejecting the
  // variant made every chat with one pending unloadable (#1712), because one
  // invalid rung fails the whole hydration response.
  it("accepts the path_prefix rungs a write_file approval offers", () => {
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "write_file",
        approval: "workspace_may_modify_files",
        class: "workspace",
        preview: { tool: "write_file", path: "data/parser.js" },
        grant_rungs: [
          { path_prefix: { segments: 2 } },
          { path_prefix: { segments: 1 } },
        ],
      })?.grantRungs,
    ).toEqual([
      { path_prefix: { segments: 2 } },
      { path_prefix: { segments: 1 } },
    ]);
    expect(
      parsePendingToolApproval({
        ...safe,
        grant_rungs: [{ path_prefix: { segments: 0 } }],
      }),
    ).toBeNull();
  });

  it("drops a preview it cannot fully validate rather than half-rendering it", () => {
    const exec = {
      ...safe,
      action: "exec",
      approval: "exec_may_run_networked_command",
    };
    for (const preview of [
      { tool: "shell", command: "rm", args: [], cwd: ".", files: [] },
      { tool: "exec", command: "", args: [], cwd: ".", files: [] },
      { tool: "exec", command: "cargo", args: "test", cwd: ".", files: [] },
      { tool: "exec", command: "cargo", args: [{ hidden: true }], cwd: ".", files: [] },
      { tool: "exec", command: "cargo", args: [] },
      "cargo test",
    ]) {
      expect(parsePendingToolApproval({ ...exec, preview })?.preview).toBeNull();
    }
  });

  it("recovers an approvable escaping exec action", () => {
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "exec",
        approval: "exec_may_run_networked_command",
        can_approve: true,
      }),
    ).toMatchObject({
      action: "exec",
      approval: "exec_may_run_networked_command",
      canApprove: true,
    });
    // The approvable invariant still binds: a presentable escaping kind that
    // claims it cannot be approved is rejected.
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "exec",
        approval: "exec_may_run_networked_command",
        can_approve: false,
      }),
    ).toBeNull();
  });

  it("recovers the narrow foreground web-search approval", () => {
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "web_search",
        approval: "web_search_may_share_query",
      }),
    ).toMatchObject({
      action: "web_search",
      approval: "web_search_may_share_query",
      canApprove: true,
    });
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "web_search",
        approval: "web_search_may_share_query",
        can_approve: false,
      }),
    ).toBeNull();
  });

  it("recovers one-shot MCP approval without a standing grant", () => {
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "other",
        approval: "external_mcp_may_call_server",
        can_remember: false,
        grant_rungs: [],
      }),
    ).toMatchObject({
      action: "other",
      approval: "external_mcp_may_call_server",
      canApprove: true,
      canRemember: false,
    });
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "other",
        approval: "external_mcp_may_call_server",
        grant_rungs: [],
      }),
    ).toBeNull();
  });

  it("recognizes the fixed background wait name without accepting extensions", () => {
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "wait_for_agents",
        approval: "unsupported",
        can_approve: false,
        can_remember: false,
        grant_rungs: [],
      }),
    ).toMatchObject({ action: "wait_for_agents", canApprove: false });
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "wait_for_agents_with_private_results",
        approval: "unsupported",
        can_approve: false,
        can_remember: false,
        grant_rungs: [],
      }),
    ).toBeNull();
  });

  it("recognizes only the fixed delegated-file renderer name", () => {
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "read_delegated_file",
        approval: "unsupported",
        can_approve: false,
        can_remember: false,
        grant_rungs: [],
      }),
    ).toMatchObject({ action: "read_delegated_file", canApprove: false });
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "read_delegated_file:/Users/private/document.txt",
        approval: "unsupported",
        can_approve: false,
        can_remember: false,
        grant_rungs: [],
      }),
    ).toBeNull();
  });

  // A parked spawn approval used to make its chat unreopenable: the kind was
  // absent from the renderer's approval vocabulary, so the row failed to parse
  // and one unparseable row fails the whole hydration response.
  it("recovers a parked background-agent spawn approval", async () => {
    const spawn = {
      ...safe,
      action: "spawn_sandbox_agent",
      approval: "delegate_may_run_background_agent",
      preview: {
        tool: "delegate_agent",
        task: "summarize the filings",
        network: {
          mode: "allowed_hosts",
          allowed_hosts: ["sec.gov"],
          package_managers: false,
        },
      },
    };
    expect(parsePendingToolApproval(spawn)).toMatchObject({
      action: "spawn_sandbox_agent",
      approval: "delegate_may_run_background_agent",
      canApprove: true,
      canRemember: true,
      preview: {
        tool: "delegate_agent",
        task: "summarize the filings",
        network: {
          mode: "allowed_hosts",
          allowed_hosts: ["sec.gov"],
          package_managers: false,
        },
      },
    });

    const client = new ApiClient("http://127.0.0.1", "token");
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify([spawn]), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      ),
    );
    await expect(client.listPendingApprovals("chat-1")).resolves.toHaveLength(1);
  });

  it("fails closed on malformed, duplicate, or cross-turn pages", async () => {
    const client = new ApiClient("http://127.0.0.1", "token");
    for (const body of [
      { approval: safe },
      [{ ...safe, arguments: { query: "private" } }],
      [safe, safe],
      [safe, { ...safe, call_id: "call-2", turn_id: "turn-2" }],
    ]) {
      vi.stubGlobal(
        "fetch",
        vi.fn().mockResolvedValue(
          new Response(JSON.stringify(body), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          }),
        ),
      );
      await expect(client.listPendingApprovals("chat-1")).rejects.toThrow(
        /pending approval response/,
      );
    }
  });
});

describe("parseToolActionPreview", () => {
  const filtered = {
    tool: "web_search",
    query: "quarterly filings",
    domains: ["sec.gov"],
    start_published_at: "2024-01-01T00:00:00Z",
    end_published_at: null,
  };

  it("keeps the filters that go to the provider with the query", () => {
    expect(parseToolActionPreview(filtered)).toEqual(filtered);
  });

  it("drops a preview whose filters it cannot verify", () => {
    // A card that describes the wrong action is worse than one that describes
    // no action, and the filters are part of the action being consented to.
    for (const broken of [
      { ...filtered, domains: "sec.gov" },
      { ...filtered, domains: [7] },
      { ...filtered, domains: undefined },
      { ...filtered, start_published_at: 20240101 },
      { ...filtered, end_published_at: undefined },
    ]) {
      expect(parseToolActionPreview(broken)).toBeNull();
    }
  });

  it("leaves a source search to its query alone", () => {
    // The private-source search has no filters to show, and inventing keys for
    // it would put a web search's copy on a local one.
    expect(parseToolActionPreview({ tool: "search", query: "revenue" })).toEqual({
      tool: "search",
      query: "revenue",
    });
  });

  it("names the file a write lands in", () => {
    expect(
      parseToolActionPreview({ tool: "write_file", path: "reports/q1.md" }),
    ).toEqual({ tool: "write_file", path: "reports/q1.md" });
    expect(parseToolActionPreview({ tool: "write_file", path: "" })).toBeNull();
  });

  it("keeps the network policy a delegated run inherits", () => {
    // The task says what the run does; the policy is the egress being
    // approved, so a preview missing or misdescribing it is dropped rather
    // than shown as a narrower run than it is.
    const delegated = {
      tool: "delegate_agent",
      task: "reconcile the ledger",
      network: {
        mode: "allowed_hosts",
        allowed_hosts: ["api.example.com"],
        package_managers: true,
      },
    };
    expect(parseToolActionPreview(delegated)).toEqual(delegated);
    expect(
      parseToolActionPreview({
        tool: "delegate_agent",
        task: "reconcile the ledger",
        network: { mode: "open" },
      }),
    ).toEqual({
      tool: "delegate_agent",
      task: "reconcile the ledger",
      network: { mode: "open" },
    });
    for (const broken of [
      { ...delegated, task: "" },
      { ...delegated, network: undefined },
      { ...delegated, network: { mode: "everything" } },
      { ...delegated, network: { mode: "allowed_hosts", allowed_hosts: ["a"] } },
      {
        ...delegated,
        network: {
          mode: "allowed_hosts",
          allowed_hosts: "api.example.com",
          package_managers: true,
        },
      },
    ]) {
      expect(parseToolActionPreview(broken)).toBeNull();
    }
  });
});

describe("sending a message", () => {
  it("carries the invoked skills beside the prose the reader typed", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await client.postMessage(
      "chat-1",
      "turn-1",
      "make the deck",
      [],
      ["doc-1"],
      ["pptx"],
      true,
    );

    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("http://127.0.0.1/chats/chat-1/messages");
    expect(JSON.parse(String(init.body))).toEqual({
      turn_id: "turn-1",
      content: "make the deck",
      attachments: [],
      file_attachments: ["doc-1"],
      invoked_skills: ["pptx"],
      voice_input_used: true,
      queue: false,
    });
  });
});

describe("active turn steering", () => {
  it("posts an interrupt against the exact chat, turn, and stable identity", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await client.steer(
      "chat-1",
      "turn-1",
      "steer-1",
      "change course",
      true,
      true,
      ["docx"],
    );

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("http://127.0.0.1/chats/chat-1/steer");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({
      steer_id: "steer-1",
      turn_id: "turn-1",
      content: "change course",
      interrupt: true,
      voice_input_used: true,
      invoked_skills: ["docx"],
    });
  });

  it("binds Code guidance to the exact active turn", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 202 }));
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await client.steerCodeSession("session-1", "turn-1", "change course");

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("http://127.0.0.1/code/sessions/session-1/steer");
    expect(init.method).toBe("POST");
    expect(JSON.parse(String(init.body))).toEqual({
      expected_turn_id: "turn-1",
      guidance: "change course",
    });
  });
});

describe("historical image API", () => {
  it("uses the chat bearer to fetch pixels rather than putting a token in the URL", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response("pixels", {
        status: 200,
        headers: { "Content-Type": "image/png" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    const blob = await client.getChatImageAttachment("chat/1", "image/1");

    expect(blob.type).toBe("image/png");
    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1/chats/chat%2F1/attachments/images/image%2F1",
      expect.objectContaining({
        headers: { Authorization: "Bearer token" },
      }),
    );
  });
});

describe("project-scoped conversation API", () => {
  it("creates a named project and a chat in that exact scope", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            id: "project-1",
            title: "Research",
            attachment_revision: 0,
            root_attachments: [],
            created_at: "2026-07-21T12:00:00Z",
          }),
          { status: 201 },
        ),
      )
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            id: "chat-1",
            title: null,
            model: "model-1",
            attachment_revision: 0,
            root_attachments: [],
            project_id: "project-1",
            created_at: "2026-07-21T12:00:00Z",
          }),
          { status: 201 },
        ),
      );
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await client.createProject("Research");
    await client.createChat("anthropic::model-1", "project-1");

    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      "http://127.0.0.1/projects",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ title: "Research" }),
      }),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "http://127.0.0.1/chats",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          model: "anthropic::model-1",
          project_id: "project-1",
        }),
      }),
    );
  });

  it("creates a chat already set up the way it will run", async () => {
    // The home composer has nowhere to PATCH: its choices have to ride along
    // with creation, or the first turn runs against a chat that was created
    // before the reader's choices reached it.
    const fetchMock = vi.fn(async () =>
      new Response(
        JSON.stringify({
          id: "chat-1",
          project_id: null,
          title: null,
          model: "anthropic::model-1",
          reasoning_effort: "high",
          permission_mode: "auto",
          network_policy: { mode: "package_managers" },
          attachment_revision: 0,
          root_attachments: [],
          created_at: "2026-07-29T12:00:00Z",
        }),
        { status: 201 },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await client.createChat("anthropic::model-1", null, {
      reasoningEffort: "high",
      permissionMode: "auto",
      networkPolicy: { mode: "package_managers" },
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1/chats",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          model: "anthropic::model-1",
          reasoning_effort: "high",
          permission_mode: "auto",
          network_policy: { mode: "package_managers" },
        }),
      }),
    );
  });

  it("keeps a loose chat explicitly outside project scope", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          id: "chat-1",
          title: null,
          model: null,
          attachment_revision: 0,
          root_attachments: [],
          project_id: null,
          created_at: "2026-07-21T12:00:00Z",
        }),
        { status: 201 },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await client.createChat(undefined, null);

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(String(init.body))).toEqual({});
  });

  it("renames the exact project with a bounded metadata patch", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          id: "project-1",
          title: "Renamed",
          attachment_revision: 0,
          root_attachments: [],
          created_at: "2026-07-21T12:00:00Z",
        }),
        { status: 200 },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await client.patchProjectTitle("project-1", "Renamed");

    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1/projects/project-1",
      expect.objectContaining({
        method: "PATCH",
        body: JSON.stringify({ title: "Renamed" }),
      }),
    );
  });

  it("deletes the exact project without a request body", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await client.deleteProject("project/1");

    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1/projects/project%2F1",
      expect.objectContaining({ method: "DELETE" }),
    );
    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(init.body).toBeUndefined();
  });
});

describe("code-execution configuration API", () => {
  it("updates only the fixed provider selection and bounded timeout", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          provider: "local",
          timeout_ms: 30_000,
          available: true,
        }),
        { status: 200 },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await client.putCodeExecutionConfig({
      provider: "local",
      timeout_ms: 30_000,
    });

    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1/code-execution",
      expect.objectContaining({
        method: "PUT",
        body: JSON.stringify({
          provider: "local",
          timeout_ms: 30_000,
        }),
      }),
    );
  });
});

describe("parseAgentActivityHistory", () => {
  it("keeps an entry whose typed detail it cannot vouch for, without the detail", () => {
    expect(
      parseAgentActivityHistory([
        {
          kind: "exec",
          outcome: "failed",
          at: "2026-08-05T18:37:00Z",
          detail: {
            kind: "exec",
            command: "pip",
            args: ["install", "matplotlib"],
            exit_code: 1,
          },
        },
        // A search headline on a command step: the tag is known, but it would
        // describe an action other than the one that ran.
        {
          kind: "exec",
          outcome: "completed",
          at: "2026-08-05T18:38:00Z",
          detail: { kind: "search", query: "quarterly revenue" },
        },
        // A headline the server's own projection would never emit: an unknown
        // tag, a control character, and an extra key it does not admit.
        {
          kind: "web_search",
          outcome: "completed",
          at: "2026-08-05T18:39:00Z",
          detail: { kind: "host_path", path: "/Users/someone" },
        },
        {
          kind: "web_search",
          outcome: "completed",
          at: "2026-08-05T18:40:00Z",
          detail: { kind: "search", query: "quarterly\nrevenue" },
        },
        {
          kind: "read_delegated_file",
          outcome: "completed",
          at: "2026-08-05T18:41:00Z",
          detail: { kind: "file", name: "brief.md", root_id: "private" },
        },
        // An argument vector the projection could not have produced: one
        // element is not a string, so the whole headline would misdescribe
        // what ran.
        {
          kind: "exec",
          outcome: "completed",
          at: "2026-08-05T18:42:00Z",
          detail: { kind: "exec", command: "python3", args: ["run.py", 7] },
        },
        // A bidirectional override would let the headline read in an order
        // other than the one that ran.
        {
          kind: "exec",
          outcome: "completed",
          at: "2026-08-05T18:43:00Z",
          detail: { kind: "exec", command: "rm", args: ["\u202egnp.txt"] },
        },
        // An exit status no process produced costs only itself: the command
        // is still the useful part of the row.
        {
          kind: "exec",
          outcome: "failed",
          at: "2026-08-05T18:44:00Z",
          detail: { kind: "exec", command: "make", args: [], exit_code: 1.5 },
        },
        // The captured tail is drawn as a block, so its line breaks are
        // structure rather than spoofing.
        {
          kind: "exec",
          outcome: "completed",
          at: "2026-08-05T18:45:00Z",
          detail: {
            kind: "exec",
            command: "make",
            args: [],
            output: "exit: 0\n\nstdout:\nbuilt 3 charts",
          },
        },
        // An escape sequence in that block could repaint the pane, so the
        // output is dropped while the row it describes survives.
        {
          kind: "exec",
          outcome: "completed",
          at: "2026-08-05T18:46:00Z",
          detail: {
            kind: "exec",
            command: "make",
            args: [],
            output: "\u001b[2Jexit: 0",
          },
        },
        // The step's own sentence is admitted, and admitting it must not cost
        // the row: a parser that did not know the key would reject the whole
        // detail and blank the command, output, and exit status with it.
        {
          kind: "exec",
          outcome: "completed",
          at: "2026-08-05T18:47:00Z",
          detail: {
            kind: "exec",
            command: "python3",
            args: ["report.py"],
            summary: "Rebuilding the quarterly report",
          },
        },
        // An unusable sentence costs only itself; the command still describes
        // the row.
        {
          kind: "exec",
          outcome: "completed",
          at: "2026-08-05T18:48:00Z",
          detail: {
            kind: "exec",
            command: "python3",
            args: [],
            summary: "x".repeat(201),
          },
        },
      ]),
    ).toEqual([
      {
        kind: "exec",
        outcome: "failed",
        at: "2026-08-05T18:37:00Z",
        detail: {
          kind: "exec",
          command: "pip",
          args: ["install", "matplotlib"],
          exit_code: 1,
        },
      },
      { kind: "exec", outcome: "completed", at: "2026-08-05T18:38:00Z" },
      { kind: "web_search", outcome: "completed", at: "2026-08-05T18:39:00Z" },
      { kind: "web_search", outcome: "completed", at: "2026-08-05T18:40:00Z" },
      {
        kind: "read_delegated_file",
        outcome: "completed",
        at: "2026-08-05T18:41:00Z",
      },
      { kind: "exec", outcome: "completed", at: "2026-08-05T18:42:00Z" },
      { kind: "exec", outcome: "completed", at: "2026-08-05T18:43:00Z" },
      {
        kind: "exec",
        outcome: "failed",
        at: "2026-08-05T18:44:00Z",
        detail: { kind: "exec", command: "make", args: [] },
      },
      {
        kind: "exec",
        outcome: "completed",
        at: "2026-08-05T18:45:00Z",
        detail: {
          kind: "exec",
          command: "make",
          args: [],
          output: "exit: 0\n\nstdout:\nbuilt 3 charts",
        },
      },
      {
        kind: "exec",
        outcome: "completed",
        at: "2026-08-05T18:46:00Z",
        detail: { kind: "exec", command: "make", args: [] },
      },
      {
        kind: "exec",
        outcome: "completed",
        at: "2026-08-05T18:47:00Z",
        detail: {
          kind: "exec",
          command: "python3",
          args: ["report.py"],
          summary: "Rebuilding the quarterly report",
        },
      },
      {
        kind: "exec",
        outcome: "completed",
        at: "2026-08-05T18:48:00Z",
        detail: { kind: "exec", command: "python3", args: [] },
      },
    ]);
  });
});

describe("sandbox agent cancellation", () => {
  it("accepts only the closed cancellation projection", () => {
    expect(
      parseSandboxAgentCancellation({ id: "run-1", status: "cancelling" }),
    ).toEqual({ id: "run-1", status: "cancelling" });
    expect(
      parseSandboxAgentCancellation({ id: "run-1", status: "cancelled" }),
    ).toEqual({ id: "run-1", status: "cancelled" });

    for (const body of [
      { id: "run-1", status: "running" },
      { id: "", status: "cancelled" },
      { id: "run-1", status: "cancelled", lease_token: "private" },
      { id: "run-1", status: "cancelled", executor_id: "private" },
      { id: "run-1", status: "cancelled", diagnostic: "private" },
    ]) {
      expect(parseSandboxAgentCancellation(body)).toBeNull();
    }
  });

  it("posts to the exact encoded run route and parses a 202 response body", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ id: "run/1", status: "cancelling" }), {
        status: 202,
        headers: { "Content-Type": "application/json" },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await expect(client.cancelAgentRun("chat/1", "run/1")).resolves.toEqual({
      id: "run/1",
      status: "cancelling",
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "http://127.0.0.1/chats/chat%2F1/agent-runs/run%2F1/cancel",
      expect.objectContaining({ method: "POST" }),
    );
  });

  it("fails closed when the server returns another run identity", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ id: "run-2", status: "cancelled" }), {
          status: 202,
        }),
      ),
    );
    const client = new ApiClient("http://127.0.0.1", "token");

    await expect(client.cancelAgentRun("chat-1", "run-1")).rejects.toThrow(
      "sandbox cancellation response is invalid",
    );
  });

  it("rejects a valid projection returned with a status other than 202", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ id: "run-1", status: "cancelled" }), {
          status: 200,
        }),
      ),
    );
    const client = new ApiClient("http://127.0.0.1", "token");

    await expect(client.cancelAgentRun("chat-1", "run-1")).rejects.toThrow(
      "expected 202, received 200",
    );
  });
});

describe("parseToolResultPreview closed results", () => {
  it("validates and remaps exec preview image references", () => {
    expect(
      parseToolResultPreview({
        tool: "exec",
        exit_code: 0,
        timed_out: false,
        output_truncated: false,
        stdout: "ok",
        stderr: "",
        images: [
          {
            blob_id: "preview-1",
            media_type: "jpeg",
            width: 1200,
            height: 800,
            byte_len: 100,
          },
        ],
      }),
    ).toEqual({
      tool: "exec",
      exitCode: 0,
      timedOut: false,
      outputTruncated: false,
      outputs: [],
      stdout: "ok",
      stderr: "",
      images: [
        {
          attachmentId: "preview-1",
          mediaType: "image/jpeg",
          width: 1200,
          height: 800,
        },
      ],
    });
  });

  it("accepts only the closed web-search setup signal", () => {
    expect(
      parseToolResultPreview({ tool: "web_search_provider_required" }),
    ).toEqual({ tool: "web_search_provider_required" });
  });

  it("accepts a validated reference and remaps it to the app shape", () => {
    expect(
      parseToolResultPreview({
        tool: "mcp_app",
        server: "gateway",
        resource_uri: "ui://gateway/app.html",
      }),
    ).toEqual({
      tool: "mcp_app",
      server: "gateway",
      resourceUri: "ui://gateway/app.html",
    });
  });

  it("drops references that are not fully verifiable", () => {
    for (const value of [
      { tool: "mcp_app", server: "gateway" },
      { tool: "mcp_app", server: "", resource_uri: "ui://x" },
      { tool: "mcp_app", server: "gateway", resource_uri: "https://evil" },
      { tool: "mcp_app", server: "gateway", resource_uri: 7 },
      { tool: "mcp_app", server: 7, resource_uri: "ui://x" },
    ]) {
      expect(parseToolResultPreview(value)).toBeNull();
    }
  });

  it("drops rows it cannot read and counts them as not shown", () => {
    // A row the parser rejects is a result the call returned and the card is
    // not showing. Dropping it silently would leave the count saying the
    // search found three things when it found five.
    expect(
      parseToolResultPreview({
        tool: "entries",
        elided: 2,
        entries: [
          {
            kind: "file",
            label: "notes.md",
            detail: "scratch",
            meta: "1.2 KB",
            media_type: "text/markdown",
          },
          { kind: "sabotage", label: "notes.md" },
          { kind: "file", label: "" },
          { kind: "folder", label: "reports" },
        ],
        failures: [
          { label: "q4.md", error: "unreadable" },
          // No reason to show, so no row — but still counted.
          { label: "q5.md" },
        ],
      }),
    ).toEqual({
      tool: "entries",
      elided: 5,
      entries: [
        {
          kind: "file",
          label: "notes.md",
          detail: "scratch",
          meta: "1.2 KB",
          mediaType: "text/markdown",
          targetId: null,
          url: null,
        },
        {
          kind: "folder",
          label: "reports",
          detail: null,
          meta: null,
          mediaType: null,
          targetId: null,
          url: null,
        },
      ],
      failures: [{ label: "q4.md", error: "unreadable" }],
    });
  });

  it("keeps an empty list, which is a result and not the absence of one", () => {
    expect(
      parseToolResultPreview({
        tool: "entries",
        entries: [],
        failures: [],
        elided: 0,
      }),
    ).toEqual({ tool: "entries", entries: [], failures: [], elided: 0 });
  });
  // A row's address is the only projected field that can send a reader out of
  // the application, so the renderer re-checks the scheme the server admitted
  // on. A row whose address it will not vouch for keeps its title and simply
  // does not open.
  it("admits only a web address a source row can be opened by", () => {
    const entries = [
      { kind: "link", label: "Report", url: "https://sec.gov/report" },
      { kind: "link", label: "Injected", url: "javascript:alert(1)" },
      { kind: "link", label: "Local", url: "file:///etc/passwd" },
      { kind: "link", label: "Malformed", url: "not a url" },
    ];
    const preview = parseToolResultPreview({
      tool: "entries",
      entries,
      failures: [],
      elided: 0,
    });
    expect(preview?.tool).toBe("entries");
    expect(
      preview?.tool === "entries"
        ? preview.entries.map((entry) => entry.url)
        : null,
    ).toEqual(["https://sec.gov/report", null, null, null]);
  });
});

describe("source download progress", () => {
  /** A response whose body arrives in chunks, as a real transfer would. */
  function streamed(chunks: string[], headers: Record<string, string> = {}) {
    const encoder = new TextEncoder();
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
        controller.close();
      },
    });
    return new Response(body, { status: 200, headers });
  }

  const megabyte = "x".repeat(1024 * 1024);

  it("reports against the declared length, ending on the total", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        streamed([megabyte, megabyte, megabyte], {
          "Content-Length": String(3 * 1024 * 1024),
          "Content-Type": "text/markdown",
        }),
      ),
    );
    const client = new ApiClient("http://127.0.0.1", "token");
    const seen: number[] = [];

    const file = await client.getChatDocumentFile("chat-1", "doc-1", undefined, (p) =>
      seen.push(p.loaded),
    );

    // Every chunk but the last may be swallowed by the throttle; the last must
    // not be, or the bar stops short of the end.
    expect(seen.at(-1)).toBe(3 * 1024 * 1024);
    expect(file.bytes.length).toBe(3 * 1024 * 1024);
    // The stored media type has to survive being reassembled from chunks.
    expect(file.contentType).toBe("text/markdown");
  });

  it("stays quiet for a small file, whose bar would only flash", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        streamed(["small"], { "Content-Length": "5" }),
      ),
    );
    const client = new ApiClient("http://127.0.0.1", "token");
    const onProgress = vi.fn();

    const { bytes } = await client.getChatDocumentFile(
      "chat-1",
      "doc-1",
      undefined,
      onProgress,
    );

    expect(onProgress).not.toHaveBeenCalled();
    expect(new TextDecoder().decode(bytes)).toBe("small");
  });

  it("stays quiet when the response declares no length to divide by", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(streamed([megabyte, megabyte, megabyte])),
    );
    const client = new ApiClient("http://127.0.0.1", "token");
    const onProgress = vi.fn();

    const { bytes } = await client.getChatDocumentFile(
      "chat-1",
      "doc-1",
      undefined,
      onProgress,
    );

    expect(onProgress).not.toHaveBeenCalled();
    expect(bytes.length).toBe(3 * 1024 * 1024);
  });

  it("surfaces the server's own message when the transfer is refused", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        new Response(JSON.stringify({ message: "source has been deleted" }), {
          status: 410,
        }),
      ),
    );
    const client = new ApiClient("http://127.0.0.1", "token");

    await expect(client.getChatDocumentFile("chat-1", "doc-1")).rejects.toThrow(
      "410: source has been deleted",
    );
  });
});

describe("code workspace sessions", () => {
  it("lists GET /code/workspaces/{id}/sessions", async () => {
    const session = {
      id: "sess-1",
      workspace_id: "ws-1",
      harness_kind: "claude_code",
      permission_mode: "plan",
      lifecycle: "idle",
      attention: { state: { type: "working" }, source: "lifecycle" },
      unrecognized_event_count: 0,
      created_at: "2026-08-15T12:00:00.000Z",
    };
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify([session]), { status: 200 }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");

    await expect(client.listCodeWorkspaceSessions("ws-1")).resolves.toEqual([
      session,
    ]);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("http://127.0.0.1/code/workspaces/ws-1/sessions");
    expect(init.method ?? "GET").toBe("GET");
  });

  it("lists GET /code/sessions/{id}/turns", async () => {
    const turn = {
      id: "turn-1",
      session_id: "sess-1",
      ordinal: 1,
      status: "completed",
      user_input: "list the files",
      attachments: [],
      started_at: "2026-08-15T12:00:00.000Z",
      ended_at: "2026-08-15T12:00:02.000Z",
    };
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify([turn]), { status: 200 }),
    );
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");
    await expect(client.listCodeSessionTurns("sess-1")).resolves.toEqual([turn]);
    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "http://127.0.0.1/code/sessions/sess-1/turns",
    );
  });
});

describe("code workspace git flow", () => {
  it("commits, pushes, and loads the PR digest", async () => {
    const commit = {
      sha: "abc123",
      message: "first change\n\n1 file changed, 1 insertion(+), 0 deletions(-)",
      stat: { files: 1, insertions: 1, deletions: 0, truncated: false },
    };
    const push = { branch: "tidebreak/first", remote: "origin" };
    const digest = {
      dirty: false,
      unpushed: false,
      ahead: 1,
      has_upstream: true,
      suggested_commit_message: commit.message,
      gh_found: true,
      gh_authenticated: true,
      remediation: "",
    };
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(new Response(JSON.stringify(commit), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(push), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(digest), { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    const client = new ApiClient("http://127.0.0.1", "token");
    await expect(client.commitCodeWorkspace("ws-1", "first change")).resolves.toEqual(commit);
    await expect(client.pushCodeWorkspace("ws-1")).resolves.toEqual(push);
    await expect(client.getCodeWorkspacePr("ws-1")).resolves.toEqual(digest);
    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "http://127.0.0.1/code/workspaces/ws-1/git/commit",
    );
    expect(fetchMock.mock.calls[1]?.[0]).toBe(
      "http://127.0.0.1/code/workspaces/ws-1/git/push",
    );
    expect(fetchMock.mock.calls[2]?.[0]).toBe(
      "http://127.0.0.1/code/workspaces/ws-1/pr",
    );
  });
});

describe("archive force kinds", () => {
  it("treats unpushed leftover work as a force-confirm, not a dead-end toast", () => {
    expect(archiveForceKind(new HttpError(409, "unpushed", "unpushed"))).toBe(
      "unpushed",
    );
    expect(
      archiveForceKind(new HttpError(409, "uncommitted", "uncommitted")),
    ).toBe("uncommitted");
    expect(
      archiveForceKind(
        new HttpError(409, "both", "uncommitted_and_unpushed"),
      ),
    ).toBe("uncommitted_and_unpushed");
    expect(archiveForceKind(new HttpError(409, "busy", "session_running"))).toBeNull();
  });
});
