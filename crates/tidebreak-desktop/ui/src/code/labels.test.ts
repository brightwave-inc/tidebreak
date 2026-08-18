import { describe, expect, it } from "vitest";

import type { ModeCaps } from "./labels";
import {
  autoIsUnsupervised,
  createPermissionModes,
  defaultCreatePermissionMode,
  gatewayCodeModels,
  groupCodeModelOptions,
  harnessUnusableReason,
  sessionLifecycleTooltip,
} from "./labels";

function caps(
  plan_mode: ModeCaps["plan_mode"],
  structured_approvals: ModeCaps["structured_approvals"],
  auto_mode: ModeCaps["auto_mode"],
  allow_mode: ModeCaps["allow_mode"] = "unsupported",
): ModeCaps {
  return { plan_mode, structured_approvals, auto_mode, allow_mode };
}

describe("sessionLifecycleTooltip", () => {
  it("joins the harness version and unread engine events", () => {
    expect(
      sessionLifecycleTooltip({
        lifecycle: "idle",
        harness: "claude_code",
        version: "2.1.233",
        unrecognizedEventCount: 3,
      }),
    ).toBe(
      "Idle · Claude Code 2.1.233 · 3 unread engine events — transcript may be incomplete",
    );
    expect(
      sessionLifecycleTooltip({
        lifecycle: "running",
        harness: "codex",
        unrecognizedEventCount: 1,
      }),
    ).toBe(
      "Running · Codex CLI · 1 unread engine event — transcript may be incomplete",
    );
  });
});

describe("create-time permission mode", () => {
  it("defaults to the most autonomous mode the engine honors", () => {
    expect(
      defaultCreatePermissionMode(caps("supported", "supported", "supported", "supported")),
    ).toBe("allow");
    expect(
      createPermissionModes(caps("supported", "supported", "supported", "supported")),
    ).toEqual(["plan", "ask", "auto", "allow"]);
    // Down the scale one flag at a time: Allow, then Auto, then Ask.
    expect(
      defaultCreatePermissionMode(caps("supported", "supported", "supported")),
    ).toBe("auto");
    expect(
      defaultCreatePermissionMode(caps("supported", "supported", "unsupported")),
    ).toBe("ask");
  });

  it("falls back to Plan when no wider mode is supported", () => {
    expect(
      defaultCreatePermissionMode(caps("supported", "unsupported", "unsupported")),
    ).toBe("plan");
    expect(
      defaultCreatePermissionMode(caps("supported", "unknown", "unknown")),
    ).toBe("plan");
    expect(
      createPermissionModes(caps("supported", "unsupported", "unsupported")),
    ).toEqual(["plan"]);
  });

  it("offers only unsupervised Auto for a grok-shaped engine", () => {
    const grok = caps("unsupported", "unsupported", "supported");
    expect(createPermissionModes(grok)).toEqual(["auto"]);
    expect(createPermissionModes(caps("unsupported", "unsupported", "supported", "supported"))).toEqual(
      ["auto", "allow"],
    );
    expect(defaultCreatePermissionMode(grok)).toBe("auto");
    expect(autoIsUnsupervised(grok)).toBe(true);
    // Supervised Auto rides the approval channel and needs no statement.
    expect(autoIsUnsupervised(caps("supported", "supported", "supported"))).toBe(
      false,
    );
  });
});

describe("harnessUnusableReason", () => {
  it("names the one reason a picker row cannot be chosen", () => {
    expect(
      harnessUnusableReason({
        found: false,
        caps: caps("supported", "supported", "supported"),
      }),
    ).toBe("Not installed");
    expect(
      harnessUnusableReason({
        found: true,
        authenticated: false,
        caps: caps("supported", "supported", "supported"),
      }),
    ).toBe("Sign in via your terminal");
    expect(
      harnessUnusableReason({
        found: true,
        caps: caps("unsupported", "unsupported", "unsupported"),
      }),
    ).toBe("Not available yet");
    expect(
      harnessUnusableReason({
        found: true,
        caps: caps("supported", "unsupported", "unsupported"),
      }),
    ).toBeNull();
    // An Auto-only engine is usable, not "Not available yet".
    expect(
      harnessUnusableReason({
        found: true,
        caps: caps("unsupported", "unsupported", "supported"),
      }),
    ).toBeNull();
  });
});

describe("gatewayCodeModels", () => {
  it("keeps only available model-gateway rows", () => {
    expect(
      gatewayCodeModels([
        {
          key: "model_gateway::claude-opus-5",
          id: "claude-opus-5",
          display_name: "Claude Opus 5",
          provider: "model_gateway",
          vendor: null,
          verification: "verified",
          recommended: true,
          available: true,
          context_window: 1,
          max_output_tokens: 1,
          input_modalities: ["text"],
          supports_reasoning: false,
          supports_tools: true,
          supports_structured_output: false,
          reasoning_efforts: [],
          supports_vision: false,
        } as never,
        {
          key: "anthropic::claude-sonnet-4",
          id: "claude-sonnet-4",
          display_name: "Claude Sonnet 4",
          provider: "anthropic",
          available: true,
        } as never,
        {
          key: "model_gateway::down",
          id: "down",
          display_name: "Down",
          provider: "model_gateway",
          available: false,
        } as never,
      ],
        "claude_code",
        "model_gateway::claude-opus-5",
      ),
    ).toEqual([
      {
        id: "claude-opus-5",
        label: "Claude Opus 5",
        source: "Claude Code · model-gateway",
        vendor: "anthropic",
        default: true,
      },
    ]);
  });

  it("confines a mixed gateway catalog to the harness's vendor", () => {
    const gatewayModel = (id: string, vendor: string | null) =>
      ({
        key: `model_gateway::${id}`,
        id,
        display_name: id,
        provider: "model_gateway",
        vendor,
        available: true,
      }) as never;
    const catalog = [
      gatewayModel("claude-opus-5", "anthropic"),
      gatewayModel("gpt-5.6-sol", "openai"),
      gatewayModel("deepseek-v4-pro", null),
      gatewayModel("grok-4.5", "xai"),
    ];
    expect(
      gatewayCodeModels(catalog, "claude_code").map((option) => option.id),
    ).toEqual(["claude-opus-5"]);
    expect(
      gatewayCodeModels(catalog, "codex").map((option) => option.id),
    ).toEqual(["gpt-5.6-sol"]);
    // opencode is vendor-neutral: the whole catalog stays.
    expect(gatewayCodeModels(catalog, "opencode")).toHaveLength(4);
  });
});

describe("groupCodeModelOptions", () => {
  it("groups rows by vendor and family, in the rail's fixed order", () => {
    const option = (id: string, vendor: string | null = null) => ({
      id,
      label: id,
      source: "opencode",
      vendor: vendor as never,
    });
    const groups = groupCodeModelOptions([
      option("deepseek-v4-pro"),
      option("claude-opus-5", "anthropic"),
      option("gpt-5.6-sol"),
      option("mystery-model"),
    ]);
    expect(groups.map((group) => [group.id, group.label])).toEqual([
      ["openai", "OpenAI"],
      ["anthropic", "Anthropic"],
      ["deepseek", "DeepSeek"],
      ["other", "Other"],
    ]);
  });
});
