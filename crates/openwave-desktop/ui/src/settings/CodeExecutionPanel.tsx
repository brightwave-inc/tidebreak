import { useEffect, useState } from "react";
import { toast } from "sonner";
import type {
  ApiClient,
  CodeExecutionConfigInfo,
  CodeExecutionCredentialReadiness,
  CodeExecutionProviderKind,
} from "../api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  ActiveProviderField,
  ProviderCredentialField,
  TimeoutSecondsField,
  timeoutMsFromSeconds,
} from "./ProviderFields";
import {
  SettingsError,
  SettingsPanel,
  SettingsSection,
  SettingsStatus,
} from "./primitives";

const MIN_CODE_EXECUTION_TIMEOUT_SECONDS = 1;
const MAX_CODE_EXECUTION_TIMEOUT_SECONDS = 120;

/** The local sandbox needs no credential, so it never appears in the key list. */
const LOCAL_PROVIDER: CodeExecutionProviderKind = "local";

export function CodeExecutionPanel({ client }: { client: ApiClient }) {
  const [config, setConfig] = useState<CodeExecutionConfigInfo | null>(null);
  const [credentials, setCredentials] = useState<
    CodeExecutionCredentialReadiness[]
  >([]);
  const [provider, setProvider] = useState<CodeExecutionProviderKind | "">("");
  const [timeoutSeconds, setTimeoutSeconds] = useState("");
  // One draft key per managed provider, so E2B and Daytona can be configured
  // together and switching the active provider discards neither.
  const [apiKeys, setApiKeys] = useState<
    Partial<Record<CodeExecutionProviderKind, string>>
  >({});
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [removing, setRemoving] = useState<CodeExecutionProviderKind | null>(
    null,
  );
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void (async () => {
      try {
        const [nextConfig, nextCredentials] = await Promise.all([
          client.getCodeExecutionConfig(),
          client.listCodeExecutionCredentials(),
        ]);
        if (cancelled) return;
        setConfig(nextConfig);
        setCredentials(nextCredentials.credentials);
        setProvider(nextConfig.provider ?? "");
        setTimeoutSeconds(String(nextConfig.timeout_ms / 1000));
      } catch (err) {
        if (!cancelled) setError(String(err));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client]);

  const working = saving || removing !== null;
  const state = codeExecutionState(config);

  async function save() {
    const timeout = timeoutMsFromSeconds(
      timeoutSeconds,
      MIN_CODE_EXECUTION_TIMEOUT_SECONDS,
      MAX_CODE_EXECUTION_TIMEOUT_SECONDS,
    );
    if ("error" in timeout) {
      setError(timeout.error);
      return;
    }

    setSaving(true);
    setError(null);
    try {
      // Keys go first so the newly active provider never lands
      // selected-but-unusable when the caller supplied both in one pass.
      for (const credential of credentials) {
        const key = apiKeys[credential.provider]?.trim();
        if (!key) continue;
        await client.putCodeExecutionCredential(credential.provider, key);
        setApiKeys((current) => ({ ...current, [credential.provider]: "" }));
      }
      const nextConfig = await client.putCodeExecutionConfig({
        provider: provider || null,
        timeout_ms: timeout.timeoutMs,
      });
      const nextCredentials = await client.listCodeExecutionCredentials();
      setConfig(nextConfig);
      setCredentials(nextCredentials.credentials);
      setProvider(nextConfig.provider ?? "");
      setTimeoutSeconds(String(nextConfig.timeout_ms / 1000));
      toast.success("Saved code-execution settings");
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  async function removeCredential(target: CodeExecutionProviderKind) {
    setRemoving(target);
    setError(null);
    try {
      await client.deleteCodeExecutionCredential(target);
      const [nextConfig, nextCredentials] = await Promise.all([
        client.getCodeExecutionConfig(),
        client.listCodeExecutionCredentials(),
      ]);
      setConfig(nextConfig);
      setCredentials(nextCredentials.credentials);
      toast.success(
        `Removed the saved ${codeExecutionProviderLabel(target)} API key`,
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setRemoving(null);
    }
  }

  return (
    <SettingsPanel
      title="Code execution"
      description="Configure as many cloud sandboxes as you like, choose where agents execute, and bound every run. Saved keys are never shown here."
      busy={loading}
    >
      {loading ? (
        <p className="text-sm text-muted-foreground">
          Loading code-execution settings…
        </p>
      ) : !config ? (
        <p className="text-sm text-muted-foreground">
          Code-execution settings are unavailable.
        </p>
      ) : (
        <>
          <SettingsStatus
            tone={state.kind}
            label={state.label}
            description={state.description}
          />

          <SettingsSection
            title="Cloud sandbox keys"
            description="Give a key to every managed provider you want available. The local sandbox needs none."
          >
            {credentials.map((credential) => (
              <ProviderCredentialField
                key={credential.provider}
                provider={codeExecutionProviderLabel(credential.provider)}
                hasCredential={credential.has_credential}
                value={apiKeys[credential.provider] ?? ""}
                disabled={working}
                removing={removing === credential.provider}
                onChange={(value) =>
                  setApiKeys((current) => ({
                    ...current,
                    [credential.provider]: value,
                  }))
                }
                onRemove={() => void removeCredential(credential.provider)}
              />
            ))}
          </SettingsSection>

          <SettingsSection
            title="Active provider"
            description="Agents execute in this one provider. The others stay configured and idle."
          >
            <ProviderAvailabilityDisclosure rows={config.providers} />

            <ActiveProviderField
              value={provider}
              disabled={working}
              onChange={setProvider}
              options={config.providers.map((row) => ({
                kind: row.provider,
                label: row.available
                  ? PROVIDER_SANDBOX_LABEL[row.provider]
                  : `${PROVIDER_SANDBOX_LABEL[row.provider]} — unavailable`,
              }))}
            />

            <TimeoutSecondsField
              label="Execution timeout"
              minSeconds={MIN_CODE_EXECUTION_TIMEOUT_SECONDS}
              maxSeconds={MAX_CODE_EXECUTION_TIMEOUT_SECONDS}
              value={timeoutSeconds}
              disabled={working}
              onChange={setTimeoutSeconds}
            />
          </SettingsSection>

          <SettingsSection
            title="Provider network enforcement"
            description="Network access is chosen per conversation beside the composer. Providers enforce that same policy with different fidelity; limitations are disclosed here."
          >
            <EgressEnforcementDisclosure
              enforcement={config.egress.enforcement}
            />
          </SettingsSection>

          <SettingsSection
            title="Detached background runs"
            description="A detached run keeps working in its sandbox even when this app is closed. It is only allowed when every safety precondition holds; until then, background runs stay attached and end with the app."
          >
            <DetachedAdmissionDisclosure rows={config.detached_admission} />
          </SettingsSection>

          {/* One save for the whole surface: it stores every key typed above
              and the selection together, so a provider cannot go active in a
              pass that failed to save its key. */}
          <div className="flex flex-wrap gap-2">
            <Button type="button" disabled={working} onClick={() => void save()}>
              {saving ? "Saving…" : "Save settings"}
            </Button>
          </div>

          <p className="text-sm leading-relaxed text-muted-foreground">
            Local execution confines writes to private chat scratch and routes
            any granted network access through a loopback broker. E2B and
            Daytona run commands in managed cloud sandboxes and reuse their
            workspace while it is alive. Every provider retains the same
            direct-command, bounded-output, idempotency, and execution-consent
            contract.
          </p>
        </>
      )}
      {error && <SettingsError>{error}</SettingsError>}
    </SettingsPanel>
  );
}

type EgressEnforcementRow =
  CodeExecutionConfigInfo["egress"]["enforcement"][number];

type DetachedAdmissionRow =
  CodeExecutionConfigInfo["detached_admission"][number];
type DetachedAdmissionDenialReason = DetachedAdmissionRow["denials"][number];

/**
 * Each typed denial reason mapped to an honest sentence: what is missing, and
 * what would change it. The reasons come from the server's real admission
 * evaluator, so this list is exactly what the gate would deny a run for —
 * the surface never composes its own judgement.
 */
const DETACHED_DENIAL_SENTENCES: Record<DetachedAdmissionDenialReason, string> =
  {
    no_scoped_model_token:
      "The model gateway can't yet issue a token limited to a single run, so a detached run would have to carry a long-lived credential. This clears when the gateway can mint run-scoped tokens.",
    no_external_lifetime_cap:
      "Nothing outside the sandbox limits how long it can live, so a run this app never reconnects to would keep running unbounded. This needs a provider that enforces a time limit at creation.",
    image_not_verified:
      "The sandbox image isn't yet verified against a trusted source before it runs.",
    host_authority_tool_surface:
      "The run's tools could reach an operation that needs your approval mid-run, and a detached run has no one to ask.",
    credentials_without_external_egress:
      "The run would carry third-party credentials without an externally enforced network boundary keeping them from leaving.",
  };

const PROVIDER_SANDBOX_LABEL: Record<
  DetachedAdmissionRow["provider"],
  string
> = {
  local: "Local native sandbox",
  e2b: "E2B cloud sandbox",
  daytona: "Daytona cloud sandbox",
};

type ProviderAvailabilityRow = CodeExecutionConfigInfo["providers"][number];
type ProviderUnavailableReason = NonNullable<
  ProviderAvailabilityRow["unavailable_reason"]
>;

/**
 * Each typed unavailability reason as the sentence that tells the user what
 * would change it. The codes come from the server's single availability
 * decision, so this list is exactly the set of states execution can be in —
 * the panel never guesses why a provider is dark.
 */
const PROVIDER_UNAVAILABLE_SENTENCES: Record<ProviderUnavailableReason, string> =
  {
    unsupported_platform:
      "Not supported on this operating system. Use a cloud sandbox instead — it runs the same commands with the same staged files.",
    missing_sandbox_binary:
      "This host is missing the sandbox binary local execution needs to confine commands.",
    missing_credential: "Add an API key above to make this provider usable.",
  };

/**
 * Per-provider capability, stated plainly: which providers could run here at
 * all, and for each one that can't, the reason and the fix. Without this a
 * host with no usable provider looks identical to a misconfigured one.
 */
function ProviderAvailabilityDisclosure({
  rows,
}: {
  rows: CodeExecutionConfigInfo["providers"];
}) {
  return (
    <div className="flex flex-col gap-2">
      {rows.map((row) => (
        <div key={row.provider} className="flex items-start gap-2 text-sm">
          <Badge variant={row.available ? "success" : "warning"} size="sm">
            {row.available ? "Available" : "Unavailable"}
          </Badge>
          <span className="text-muted-foreground">
            <span className="font-medium text-foreground">
              {PROVIDER_SANDBOX_LABEL[row.provider]}
            </span>
            {row.unavailable_reason &&
              ` — ${PROVIDER_UNAVAILABLE_SENTENCES[row.unavailable_reason]}`}
          </span>
        </div>
      ))}
    </div>
  );
}

/**
 * Per-provider detached-admission disclosure: for each execution provider,
 * whether a detached run would be allowed, and — when it wouldn't — every
 * unmet precondition in plain language, so the user can see why detached
 * execution is unavailable and what would change it.
 */
function DetachedAdmissionDisclosure({
  rows,
}: {
  rows: CodeExecutionConfigInfo["detached_admission"];
}) {
  return (
    <div className="flex flex-col gap-3">
      {rows.map((row) => (
        <div key={row.provider} className="flex flex-col gap-1 text-sm">
          <div className="flex items-center gap-2">
            <Badge variant={row.admitted ? "success" : "warning"} size="sm">
              {row.admitted ? "Available" : "Not available"}
            </Badge>
            <span className="font-medium text-foreground">
              {PROVIDER_SANDBOX_LABEL[row.provider]}
            </span>
          </div>
          {!row.admitted && (
            <ul className="ml-5 list-disc text-muted-foreground">
              {row.denials.map((denial) => (
                <li key={denial}>{DETACHED_DENIAL_SENTENCES[denial]}</li>
              ))}
            </ul>
          )}
        </div>
      ))}
    </div>
  );
}


/**
 * How each enforcement status reads: the badge never claims a boundary a
 * provider does not have, and the caveat sits inline beside it rather than in
 * prose below. Driven by the server's status, which is itself derived from the
 * enforcement model, so the UI cannot oversell what the model won't back.
 */
const EGRESS_STATUS_PRESENTATION: Record<
  EgressEnforcementRow["status"],
  { badge: "success" | "info" | "warning" | "critical"; label: string; lead: string }
> = {
  boundary: {
    badge: "success",
    label: "Boundary",
    lead: "Enforced as a full network boundary.",
  },
  // A real boundary, but gated on a precondition the host can't verify. Shown
  // in the informational tone rather than plain green, with the requirement
  // rendered inline below, so it never reads as an unconditional boundary.
  conditional_boundary: {
    badge: "info",
    label: "Boundary — conditional",
    lead: "A strict per-sandbox boundary when enforced: unlisted domains, raw IPs, and unlisted-domain DNS are all blocked — stronger than E2B. Not guaranteed on every account:",
  },
  applied_with_gaps: {
    badge: "warning",
    label: "Applied — not a full boundary",
    lead: "The allowlist is applied, but these stay reachable regardless of policy:",
  },
  unconfirmed: {
    badge: "warning",
    label: "Unconfirmed",
    lead: "A policy is sent at creation, but enforcement is not yet confirmed against the live API. These stay reachable regardless of policy:",
  },
};

/**
 * Per-provider egress enforcement, disclosed honestly: the badge reflects the
 * model's own status and the gaps the vendor leaves open are listed inline, so
 * a provider that is not a full boundary can never read as one.
 */
function EgressEnforcementDisclosure({
  enforcement,
}: {
  enforcement: CodeExecutionConfigInfo["egress"]["enforcement"];
}) {
  return (
    <div className="flex flex-col gap-2">
      {enforcement.map((row) => {
        const presentation = EGRESS_STATUS_PRESENTATION[row.status];
        return (
          <div key={row.provider} className="flex items-start gap-2 text-sm">
            <Badge variant={presentation.badge} size="sm">
              {presentation.label}
            </Badge>
            <span className="text-muted-foreground">
              <span className="font-medium text-foreground">
                {codeExecutionProviderLabel(row.provider)}
              </span>{" "}
              — {presentation.lead}
              {row.gaps.length > 0 && ` ${row.gaps.join("; ")}.`}
              {row.requirement && (
                <span className="font-medium text-foreground">
                  {" "}
                  Requires {row.requirement}. On a lower tier the per-sandbox
                  override is refused and the account default applies.
                </span>
              )}
            </span>
          </div>
        );
      })}
    </div>
  );
}

function codeExecutionState(config: CodeExecutionConfigInfo | null): {
  kind: "disabled" | "ready" | "not-configured";
  label: string;
  description: string;
} {
  if (!config?.provider) {
    // With no provider selected, say whether that is a choice or the only
    // honest state this host has: if nothing here could run, the reasons in
    // the list below are the whole story.
    const usable = config?.providers.some((row) => row.available) ?? true;
    return {
      kind: "disabled",
      label: usable ? "Disabled" : "No execution provider configured",
      description: usable
        ? "No code-execution provider is selected."
        : "No execution provider is available on this host. Add a cloud sandbox key below to enable code execution.",
    };
  }
  if (config.available) {
    return {
      kind: "ready",
      label: "Ready",
      description:
        config.provider !== LOCAL_PROVIDER
          ? `${codeExecutionProviderLabel(config.provider)} is selected and has a saved credential.`
          : "The local native sandbox is available.",
    };
  }
  return {
    kind: "not-configured",
    label: "Unavailable",
    description: `${codeExecutionProviderLabel(config.provider)} is selected but cannot run: ${
      config.unavailable_reason
        ? PROVIDER_UNAVAILABLE_SENTENCES[config.unavailable_reason]
        : "the provider reported no reason."
    }`,
  };
}

function codeExecutionProviderLabel(
  provider: CodeExecutionProviderKind,
): string {
  switch (provider) {
    case "local":
      return "Local";
    case "e2b":
      return "E2B";
    case "daytona":
      return "Daytona";
  }
}
