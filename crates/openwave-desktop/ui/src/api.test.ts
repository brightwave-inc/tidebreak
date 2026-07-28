import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiClient,
  parseFolderAccessRequest,
  parsePendingChatPrompt,
  parseOutputWritebackRequest,
  parsePendingUserQuestions,
  parsePendingToolApproval,
  parseSandboxAgentCancellation,
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

describe("pending chat prompt summaries", () => {
  const safe = {
    chat_id: "chat-1",
    question_call_ids: ["question-1"],
    folder_access_call_ids: ["folder-1"],
    output_writeback_call_ids: ["writeback-1"],
  };

  it("accepts only opaque prompt identities", () => {
    expect(parsePendingChatPrompt(safe)).toEqual({
      chatId: "chat-1",
      questionCallIds: ["question-1"],
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
        folderAccessCallIds: ["folder-1"],
        outputWritebackCallIds: ["writeback-1"],
      },
    ]);
    expect(fetchMock.mock.calls[0][0]).toBe("http://127.0.0.1/chats/pending-prompts");
  });
});

describe("parseOutputWritebackRequest", () => {
  it("accepts only opaque identities and claimed state", () => {
    expect(
      parseOutputWritebackRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        claimed: false,
      }),
    ).toEqual({
      callId: "call-1",
      turnId: "turn-1",
      claimedByDesktop: false,
    });
    expect(
      parseOutputWritebackRequest({
        call_id: "call-1",
        turn_id: "turn-1",
        claimed: false,
        path: "private/file.txt",
      }),
    ).toBeNull();
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
    });
    expect(parsePendingToolApproval({ ...safe, arguments: { query: "private" } })).toBeNull();
    expect(parsePendingToolApproval({ ...safe, can_approve: false })).toBeNull();
    expect(parsePendingToolApproval({ ...safe, action: "private_plugin" })).toBeNull();
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
        },
      })?.preview,
    ).toEqual({
      tool: "exec",
      command: "cargo",
      args: ["test", "--workspace"],
      cwd: "checkout",
    });
  });

  it("drops a preview it cannot fully validate rather than half-rendering it", () => {
    const exec = {
      ...safe,
      action: "exec",
      approval: "exec_may_run_networked_command",
    };
    for (const preview of [
      { tool: "shell", command: "rm", args: [], cwd: "." },
      { tool: "exec", command: "", args: [], cwd: "." },
      { tool: "exec", command: "cargo", args: "test", cwd: "." },
      { tool: "exec", command: "cargo", args: [{ hidden: true }], cwd: "." },
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
      }),
    ).toMatchObject({ action: "wait_for_agents", canApprove: false });
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "wait_for_agents_with_private_results",
        approval: "unsupported",
        can_approve: false,
        can_remember: false,
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
      }),
    ).toMatchObject({ action: "read_delegated_file", canApprove: false });
    expect(
      parsePendingToolApproval({
        ...safe,
        action: "read_delegated_file:/Users/private/document.txt",
        approval: "unsupported",
        can_approve: false,
        can_remember: false,
      }),
    ).toBeNull();
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
});
