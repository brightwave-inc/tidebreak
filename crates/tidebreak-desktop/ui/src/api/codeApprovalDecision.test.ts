// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { ApiClient } from "./client";
afterEach(() => vi.unstubAllGlobals());
it("posts the structured questions decision without changing the answer IDs", async () => {
  const fetch = vi.fn(
    async () =>
      new Response(
        JSON.stringify({
          id: "a1",
          session_id: "s1",
          turn_id: "t1",
          kind: { type: "questions", questions: [] },
          state: "approved",
          requested_at: "2026-09-14T12:00:00Z",
          harness_raw_json: "",
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
  );
  vi.stubGlobal("fetch", fetch);
  const body = {
    decision: {
      answers: {
        answers: [
          {
            question_id: "q1",
            selected_option_ids: ["option-one"],
            custom_answer: "Also check logs",
          },
        ],
      },
    },
  };
  await new ApiClient("http://127.0.0.1", "token").decideCodeApproval(
    "a1",
    body,
  );
  expect(fetch).toHaveBeenCalledWith(
    "http://127.0.0.1/approvals/a1/decision",
    expect.objectContaining({ method: "POST", body: JSON.stringify(body) }),
  );
});
