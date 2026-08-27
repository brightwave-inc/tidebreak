import { describe, expect, it } from "vitest";

import type { ReasoningEffort } from "../api/types";
import type { ModeCaps } from "./labels";
import {
  ALLOW_ALL_NOTE,
  PERMISSION_MODE_POSTURES,
  UNSUPERVISED_AUTO_NOTE,
  autoIsUnsupervised,
  createPermissionModes,
  defaultCreatePermissionMode,
  effortLadder,
  gatewayCodeModels,
  groupCodeModelOptions,
  harnessCanStartNow,
  harnessNeedsDownload,
  harnessUnusableReason,
  isHarnessReady,
  preferredCodeModels,
  sessionLifecycleTooltip,
  sessionPermissionModeTooltip,
  unsupervisedModeStatement,
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
  it("joins the harness version and recorded unrecognized engine events", () => {
    expect(
      sessionLifecycleTooltip({
        lifecycle: "idle",
        harness: "claude_code",
        version: "2.1.233",
        unrecognizedEventCount: 3,
      }),
    ).toBe(
      "Idle · Claude Code 2.1.233 · 3 unrecognized engine events recorded — transcript may be incomplete",
    );
    expect(
      sessionLifecycleTooltip({
        lifecycle: "running",
        harness: "codex",
        unrecognizedEventCount: 1,
        runningLabel: "Monitoring",
      }),
    ).toBe(
      "Monitoring · Codex CLI · 1 unrecognized engine event recorded — transcript may be incomplete",
    );
  });
});

describe("create-time permission mode", () => {
  it("defaults to the most autonomous mode the engine honors", () => {
    expect(
      defaultCreatePermissionMode(
        caps("supported", "supported", "supported", "supported"),
      ),
    ).toBe("allow");
    expect(
      createPermissionModes(
        caps("supported", "supported", "supported", "supported"),
      ),
    ).toEqual(["plan", "ask", "auto", "allow"]);
    // Down the scale one flag at a time: Allow, then Auto, then Ask.
    expect(
      defaultCreatePermissionMode(caps("supported", "supported", "supported")),
    ).toBe("auto");
    expect(
      defaultCreatePermissionMode(
        caps("supported", "supported", "unsupported"),
      ),
    ).toBe("ask");
  });

  it("falls back to Plan when no wider mode is supported", () => {
    expect(
      defaultCreatePermissionMode(
        caps("supported", "unsupported", "unsupported"),
      ),
    ).toBe("plan");
    expect(
      defaultCreatePermissionMode(caps("supported", "unknown", "unknown")),
    ).toBe("plan");
    expect(
      createPermissionModes(caps("supported", "unsupported", "unsupported")),
    ).toEqual(["plan"]);
  });

  it("offers unsupervised Auto and Allow for a grok-shaped engine", () => {
    // Grok: no plan mode, no approval channel, both autonomous postures
    // (`crates/tidebreak-harness/src/grok/mod.rs`).
    const grok = caps("unsupported", "unsupported", "supported", "supported");
    expect(createPermissionModes(grok)).toEqual(["auto", "allow"]);
    expect(defaultCreatePermissionMode(grok)).toBe("allow");
    expect(autoIsUnsupervised(grok)).toBe(true);
    // An engine with an auto posture and no allow-all still lands on Auto.
    const autoOnly = caps("unsupported", "unsupported", "supported");
    expect(createPermissionModes(autoOnly)).toEqual(["auto"]);
    expect(defaultCreatePermissionMode(autoOnly)).toBe("auto");
    expect(autoIsUnsupervised(autoOnly)).toBe(true);
    // Supervised Auto rides the approval channel and needs no statement.
    expect(
      autoIsUnsupervised(caps("supported", "supported", "supported")),
    ).toBe(false);
  });
});

describe("unsupervisedModeStatement", () => {
  it("states a posture that runs with nobody to ask", () => {
    // Allow all never asks, whatever the engine can do.
    expect(unsupervisedModeStatement("allow", false)).toBe(ALLOW_ALL_NOTE);
    expect(unsupervisedModeStatement("allow", true)).toBe(ALLOW_ALL_NOTE);
    // Auto only when the engine has no approval channel to escalate through.
    expect(unsupervisedModeStatement("auto", true)).toBe(
      UNSUPERVISED_AUTO_NOTE,
    );
    expect(unsupervisedModeStatement("auto", false)).toBeNull();
    // A posture that escalates needs no statement.
    expect(unsupervisedModeStatement("ask", true)).toBeNull();
    expect(unsupervisedModeStatement("plan", true)).toBeNull();
  });
});

describe("sessionPermissionModeTooltip", () => {
  it("names the posture and spells out what it does", () => {
    expect(sessionPermissionModeTooltip("allow")).toBe(
      `Permissions: Allow all\n${PERMISSION_MODE_POSTURES.allow}`,
    );
    expect(sessionPermissionModeTooltip("plan")).toBe(
      `Permissions: Plan\n${PERMISSION_MODE_POSTURES.plan}`,
    );
  });
});

describe("harnessUnusableReason", () => {
  it("requires a positive authentication observation before an engine is ready", () => {
    expect(isHarnessReady({ found: true, authenticated: true })).toBe(true);
    expect(isHarnessReady({ found: true, authenticated: false })).toBe(false);
    expect(isHarnessReady({ found: true })).toBe(false);
  });

  it("reads a relay-covered engine as ready on a hosted machine", () => {
    // The local probe observation is not the verdict there: the relay
    // carries the turn (decision 71), signed out or not.
    expect(
      isHarnessReady({
        found: true,
        authenticated: false,
        auth_mode: "gateway_relay",
      }),
    ).toBe(true);
    expect(isHarnessReady({ found: false, auth_mode: "gateway_relay" })).toBe(
      false,
    );
    // An engine the relay does not cover can never be ready hosted.
    expect(
      isHarnessReady({
        found: true,
        authenticated: true,
        auth_mode: "hosted_unavailable",
      }),
    ).toBe(false);
  });

  it("gates hosted rows on relay coverage, not a terminal sign-in", () => {
    expect(
      harnessUnusableReason({
        found: true,
        installable: true,
        authenticated: false,
        auth_mode: "gateway_relay",
        caps: caps("supported", "supported", "supported"),
      }),
    ).toBeNull();
    expect(
      harnessUnusableReason({
        found: true,
        installable: true,
        authenticated: true,
        auth_mode: "hosted_unavailable",
        caps: caps("supported", "supported", "supported"),
      }),
    ).toBe("Not available on hosted machines yet");
  });

  it("names the one reason a picker row cannot be chosen", () => {
    expect(
      harnessUnusableReason({
        found: false,
        installable: false,
        caps: caps("supported", "supported", "supported"),
      }),
    ).toBe("Not installed");
    expect(
      harnessUnusableReason({
        found: true,
        installable: true,
        authenticated: false,
        caps: caps("supported", "supported", "supported"),
      }),
    ).toBe("Sign in via your terminal");
    expect(
      harnessUnusableReason({
        found: true,
        installable: true,
        caps: caps("supported", "supported", "supported"),
      }),
    ).toBe("Unverified — sign in via your terminal");
    expect(
      harnessUnusableReason({
        found: true,
        installable: true,
        authenticated: true,
        caps: caps("unsupported", "unsupported", "unsupported"),
      }),
    ).toBe("Not available yet");
    expect(
      harnessUnusableReason({
        found: true,
        installable: true,
        authenticated: true,
        caps: caps("supported", "unsupported", "unsupported"),
      }),
    ).toBeNull();
    // An Auto-only engine is usable, not "Not available yet".
    expect(
      harnessUnusableReason({
        found: true,
        installable: true,
        authenticated: true,
        caps: caps("unsupported", "unsupported", "supported"),
      }),
    ).toBeNull();
  });

  // The whole point of the lazy pin: an engine Tidebreak can fetch is a wait,
  // not a fault, so the picker offers it and choosing it starts the download.
  it("lets a downloadable engine be chosen", () => {
    const entry = {
      found: false,
      installable: true,
      caps: caps("supported", "supported", "supported"),
    };
    expect(harnessUnusableReason(entry)).toBeNull();
    expect(harnessNeedsDownload(entry)).toBe(true);
    expect(harnessCanStartNow(entry)).toBe(false);
    expect(
      harnessCanStartNow({ ...entry, found: true, authenticated: true }),
    ).toBe(true);
  });
});

describe("gatewayCodeModels", () => {
  it("keeps only available model-gateway rows", () => {
    expect(
      gatewayCodeModels(
        [
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
        // No `reasoning_efforts`: the row's chat-catalog ladder is not the
        // engine's, and a code session runs on the engine's.
      },
    ]);
  });

  it("falls back to the engine's ladder for a row that states none", () => {
    const engine: ReasoningEffort[] = ["low", "medium", "high", "xhigh", "max"];
    const row = (efforts?: ReasoningEffort[]) => ({
      id: "m",
      label: "M",
      source: "s",
      ...(efforts ? { reasoning_efforts: efforts } : {}),
    });

    // A gateway row carries no ladder of its own, so the engine's applies.
    expect(effortLadder(row(), engine)).toEqual(engine);
    expect(effortLadder(row([]), engine)).toEqual(engine);
    // A row the engine listed itself narrows the offer: Codex advertises a
    // different ladder per model, and only some rows reach the top rung.
    expect(effortLadder(row(["low", "high"]), engine)).toEqual(["low", "high"]);
    // An engine with no effort control at all offers nothing.
    expect(effortLadder(row(), [])).toEqual([]);
    expect(effortLadder(undefined, engine)).toEqual(engine);
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

describe("preferredCodeModels", () => {
  const native = [
    {
      id: "model-gateway/deepseek-v4-pro",
      label: "DeepSeek V4 Pro",
      source: "opencode",
    },
  ];
  const gateway = [
    {
      id: "accounts/fireworks/models/deepseek-v4-pro",
      label: "DeepSeek V4 Pro",
      source: "opencode · model-gateway",
    },
  ];

  it("uses provider-qualified ids for engines that require their own listing", () => {
    expect(preferredCodeModels("opencode", native, gateway)).toEqual(native);
    expect(preferredCodeModels("opencode", [], gateway)).toEqual(gateway);
    const grok = [
      {
        id: "model-gateway-model-gateway/grok-4.6",
        label: "Grok 4.6",
        source: "Grok CLI",
      },
    ];
    expect(preferredCodeModels("grok", grok, gateway)).toEqual(grok);
  });

  it("keeps gateway rows for engines that accept their ids directly", () => {
    expect(preferredCodeModels("codex", native, gateway)).toEqual(gateway);
    expect(preferredCodeModels("claude_code", native, [])).toEqual(native);
  });

  it("copies fast_mode from the harness listing onto matching gateway rows", () => {
    const listed = [
      {
        id: "claude-opus-5",
        label: "Claude Opus 5",
        source: "Claude Code",
        fast_mode: true,
      },
    ];
    const catalog = [
      {
        id: "claude-opus-5",
        label: "Claude Opus 5",
        source: "Claude Code · model-gateway",
      },
      {
        id: "claude-sonnet-5",
        label: "Claude Sonnet 5",
        source: "Claude Code · model-gateway",
      },
    ];
    expect(preferredCodeModels("claude_code", listed, catalog)).toEqual([
      {
        id: "claude-opus-5",
        label: "Claude Opus 5",
        source: "Claude Code · model-gateway",
        fast_mode: true,
      },
      {
        id: "claude-sonnet-5",
        label: "Claude Sonnet 5",
        source: "Claude Code · model-gateway",
      },
    ]);
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
