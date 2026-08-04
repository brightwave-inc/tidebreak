// @vitest-environment jsdom

import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ApiClient,
  CodeExecutionConfigInfo,
  CodeExecutionCredentialReadiness,
} from "../api";
import { CodeExecutionPanel } from "./CodeExecutionPanel";

/**
 * The default egress projection: open policy, E2B applied-with-gaps, Daytona a
 * strict per-sandbox boundary conditional on org tier 3+ (no phantom gaps).
 */
const OPEN_EGRESS: CodeExecutionConfigInfo["egress"] = {
  policy: { mode: "open" },
  enforcement: [
    {
      provider: "e2b",
      status: "applied_with_gaps",
      gaps: ["DNS resolution", "domain filtering covers HTTP and HTTPS ports only"],
    },
    {
      provider: "daytona",
      status: "conditional_boundary",
      gaps: [],
      requirement: "Daytona org tier 3+",
    },
  ],
};

/**
 * Today's server position: no provider is admitted for detached runs, and the
 * local row names the three structural gaps.
 */
const NO_DETACHED: CodeExecutionConfigInfo["detached_admission"] = [
  {
    provider: "local",
    admitted: false,
    denials: [
      "no_scoped_model_token",
      "no_external_lifetime_cap",
      "image_not_verified",
    ],
  },
  {
    provider: "e2b",
    admitted: false,
    denials: [
      "no_scoped_model_token",
      "no_external_lifetime_cap",
      "image_not_verified",
    ],
  },
  {
    provider: "daytona",
    admitted: false,
    denials: [
      "no_scoped_model_token",
      "no_external_lifetime_cap",
      "image_not_verified",
    ],
  },
];

/** Every provider usable: the macOS shape, where nothing is dark. */
const ALL_PROVIDERS_AVAILABLE: CodeExecutionConfigInfo["providers"] = [
  { provider: "local", available: true },
  { provider: "e2b", available: true },
  { provider: "daytona", available: true },
];

function clientFor(
  config: Omit<CodeExecutionConfigInfo, "providers"> &
    Partial<Pick<CodeExecutionConfigInfo, "providers">>,
  credentials: CodeExecutionCredentialReadiness[] = [
    { provider: "e2b", has_credential: false },
    { provider: "daytona", has_credential: false },
  ],
) {
  const resolved: CodeExecutionConfigInfo = {
    providers: ALL_PROVIDERS_AVAILABLE,
    ...config,
  };
  const putCodeExecutionConfig = vi.fn().mockResolvedValue(resolved);
  const putCodeExecutionCredential = vi
    .fn()
    .mockImplementation((provider: string) =>
      Promise.resolve({ provider, has_credential: true }),
    );
  const deleteCodeExecutionCredential = vi
    .fn()
    .mockImplementation((provider: string) =>
      Promise.resolve({ provider, has_credential: false }),
    );
  return {
    client: {
      getCodeExecutionConfig: vi.fn().mockResolvedValue(resolved),
      listCodeExecutionCredentials: vi.fn().mockResolvedValue({ credentials }),
      putCodeExecutionConfig,
      putCodeExecutionCredential,
      deleteCodeExecutionCredential,
    } as unknown as ApiClient,
    putCodeExecutionConfig,
    putCodeExecutionCredential,
    deleteCodeExecutionCredential,
  };
}

afterEach(cleanup);

describe("CodeExecutionPanel", () => {
  it("saves a key per managed provider before the active selection", async () => {
    const { client, putCodeExecutionConfig, putCodeExecutionCredential } =
      clientFor({
        provider: "e2b",
        timeout_ms: 20_000,
        available: false,
        has_credential: false,
        egress: OPEN_EGRESS,
      detached_admission: NO_DETACHED,
      });

    render(<CodeExecutionPanel client={client} />);

    fireEvent.change(await screen.findByLabelText(/E2B API key/), {
      target: { value: "  e2b-secret  " },
    });
    fireEvent.change(screen.getByLabelText(/Daytona API key/), {
      target: { value: "daytona-secret" },
    });
    fireEvent.change(screen.getByLabelText(/Execution timeout/), {
      target: { value: "30" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    await waitFor(() =>
      expect(putCodeExecutionCredential).toHaveBeenCalledWith(
        "e2b",
        "e2b-secret",
      ),
    );
    expect(putCodeExecutionCredential).toHaveBeenCalledWith(
      "daytona",
      "daytona-secret",
    );
    expect(putCodeExecutionConfig).toHaveBeenCalledWith({
      provider: "e2b",
      timeout_ms: 30_000,
    });
    // A provider must not go active in a pass that failed to store its key.
    expect(
      putCodeExecutionCredential.mock.invocationCallOrder[0],
    ).toBeLessThan(putCodeExecutionConfig.mock.invocationCallOrder[0]);
  });

  it("removes one provider's saved key without touching the other", async () => {
    const { client, deleteCodeExecutionCredential } = clientFor(
      {
        provider: "e2b",
        timeout_ms: 20_000,
        available: true,
        has_credential: true,
        egress: OPEN_EGRESS,
      detached_admission: NO_DETACHED,
      },
      [
        { provider: "e2b", has_credential: true },
        { provider: "daytona", has_credential: true },
      ],
    );

    render(<CodeExecutionPanel client={client} />);

    fireEvent.click(
      await screen.findByRole("button", { name: "Remove saved Daytona key" }),
    );

    await waitFor(() =>
      expect(deleteCodeExecutionCredential).toHaveBeenCalledWith("daytona"),
    );
    expect(deleteCodeExecutionCredential).toHaveBeenCalledTimes(1);
  });

  it("does not present E2B as a full boundary and surfaces its gaps inline", async () => {
    const { client } = clientFor({
      provider: "e2b",
      timeout_ms: 20_000,
      available: true,
      has_credential: true,
      egress: OPEN_EGRESS,
      detached_admission: NO_DETACHED,
    });

    render(<CodeExecutionPanel client={client} />);

    // The E2B badge must read "not a full boundary", never a plain green
    // "Enforced"/boundary, and the reachable holes are shown next to it.
    await screen.findByText(/not a full boundary/i);
    expect(screen.queryByText(/^Boundary$/)).toBeNull();
    expect(
      screen.getByText(/domain filtering covers HTTP and HTTPS ports only/i),
    ).toBeInTheDocument();
  });

  it("presents Daytona as a conditional boundary requiring org tier 3+, never an unconditional boundary", async () => {
    const { client } = clientFor({
      provider: "daytona",
      timeout_ms: 20_000,
      available: true,
      has_credential: true,
      egress: OPEN_EGRESS,
      detached_admission: NO_DETACHED,
    });

    render(<CodeExecutionPanel client={client} />);

    // The tier-3+ requirement is surfaced inline, and the badge reads
    // conditional — never a plain green "Boundary".
    await screen.findByText(/Requires Daytona org tier 3\+/i);
    expect(screen.getByText(/Boundary — conditional/i)).toBeInTheDocument();
    expect(screen.queryByText(/^Boundary$/)).toBeNull();
    // The phantom curated-service exceptions are gone from the disclosure.
    expect(screen.queryByText(/git hosting/i)).toBeNull();
  });

  it("rejects a timeout outside the bounds before touching the server", async () => {
    const { client, putCodeExecutionConfig } = clientFor({
      provider: "local",
      timeout_ms: 20_000,
      available: true,
      has_credential: false,
      egress: OPEN_EGRESS,
      detached_admission: NO_DETACHED,
    });

    render(<CodeExecutionPanel client={client} />);

    fireEvent.change(await screen.findByLabelText(/Execution timeout/), {
      target: { value: "500" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save settings" }));

    await screen.findByRole("alert");
    expect(putCodeExecutionConfig).not.toHaveBeenCalled();
  });

  it("explains per provider why detached runs are unavailable, in plain language", async () => {
    const { client } = clientFor({
      provider: "local",
      timeout_ms: 20_000,
      available: true,
      has_credential: false,
      egress: OPEN_EGRESS,
      detached_admission: NO_DETACHED,
    });

    render(<CodeExecutionPanel client={client} />);

    // One "Not available" badge per provider, and the typed denial reasons
    // are rendered as honest sentences — never raw enum names.
    expect(await screen.findAllByText("Not available")).toHaveLength(3);
    expect(
      screen.getAllByText(/token limited to a single run/i).length,
    ).toBeGreaterThan(0);
    expect(
      screen.getAllByText(/limits how long it can live/i).length,
    ).toBeGreaterThan(0);
    expect(
      screen.getAllByText(/isn't yet verified against a trusted source/i)
        .length,
    ).toBeGreaterThan(0);
    expect(screen.queryByText(/no_scoped_model_token/)).toBeNull();
  });

  it("names why each provider is unusable on a host with no execution at all", async () => {
    const { client } = clientFor({
      provider: undefined,
      timeout_ms: 20_000,
      available: false,
      has_credential: false,
      providers: [
        {
          provider: "local",
          available: false,
          unavailable_reason: "unsupported_platform",
        },
        {
          provider: "e2b",
          available: false,
          unavailable_reason: "missing_credential",
        },
        {
          provider: "daytona",
          available: false,
          unavailable_reason: "missing_credential",
        },
      ],
      egress: OPEN_EGRESS,
      detached_admission: NO_DETACHED,
    });

    render(<CodeExecutionPanel client={client} />);

    // The headline states the host has nothing, and each row says what would
    // change it — a missing key must be readable here, not archaeological.
    await screen.findByText(/No execution provider configured/i);
    expect(
      screen.getByText(/Not supported on this operating system/i),
    ).toBeInTheDocument();
    expect(screen.getAllByText(/Add an API key above/i)).toHaveLength(2);
    expect(screen.queryByText(/missing_credential/)).toBeNull();
  });
});
