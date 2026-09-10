import { useEffect, useRef, useState } from "react";
import type { ApiClient, HarnessKind } from "../api";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  SettingsError,
  SettingsField,
  SettingsPanel,
  SettingsSection,
} from "./primitives";
import type {
  ChannelPreferences,
  ChannelPreferencesSnapshot,
} from "./channelPreferences";

const LABELS: Record<HarnessKind, string> = {
  internal: "Tidebreak",
  claude_code: "Claude Code",
  codex: "Codex",
  opencode: "OpenCode",
  grok: "Grok",
};

export function ChannelPreferencesPanel({
  client,
  grantId,
  channelId,
}: {
  client: ApiClient;
  grantId: string;
  channelId: string;
}) {
  const [preferences, setPreferences] =
    useState<ChannelPreferencesSnapshot | null>(null);
  const [harnesses, setHarnesses] = useState<HarnessKind[]>([]);
  const [models, setModels] = useState<{ id: string; label: string }[]>([]);
  const [instructions, setInstructions] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const generation = useRef(0);
  useEffect(() => {
    const current = ++generation.current;
    setLoading(true);
    setSaving(false);
    setPreferences(null);
    setError(null);
    void client
      .getChannelPreferences(grantId, channelId)
      .then((next) => {
        if (generation.current !== current) return;
        setPreferences(next);
        setInstructions(next.instructions);
      })
      .catch((err: unknown) => {
        if (generation.current === current) setError(String(err));
      })
      .finally(() => {
        if (generation.current === current) setLoading(false);
      });
    return () => {
      generation.current++;
    };
  }, [client, grantId, channelId, reload]);
  useEffect(() => {
    let active = true;
    setModels([]);
    setCatalogError(null);
    const harness = preferences?.harness;
    if (preferences)
      void client
        .getChannelHarnessCatalog(grantId, channelId)
        .then(async (available) => {
          if (!active) return [];
          setHarnesses(available.harnesses);
          if (!harness) return [];
          if (!available.harnesses.includes(harness)) {
            throw new Error(
              "The saved harness is unavailable for this channel. Choose an available harness.",
            );
          }
          const catalog = await client.getChannelHarnessCatalog(
            grantId,
            channelId,
            harness,
          );
          if (!catalog.use_chat_catalog) return catalog.models;
          const chat = await client.listModels();
          return chat.models
            .filter((model) => model.available && model.supports_tools)
            .map((model) => ({ id: model.key, label: model.display_name }));
        })
        .then((models) => {
          if (active) setModels(models);
        })
        .catch((err: unknown) => {
          if (active) setCatalogError(String(err));
        });
    return () => {
      active = false;
    };
  }, [client, grantId, channelId, preferences?.harness]);
  async function save(changes: Partial<ChannelPreferences>) {
    if (!preferences || saving || !preferences.can_edit) return;
    const current = generation.current;
    setSaving(true);
    setError(null);
    const {
      harness,
      model,
      respond_automatically,
      instructions: savedInstructions,
    } = preferences;
    try {
      const next = await client.setChannelPreferences(grantId, channelId, {
        harness,
        model,
        respond_automatically,
        instructions: savedInstructions,
        ...changes,
      });
      if (generation.current === current) setPreferences(next);
    } catch (err) {
      if (generation.current === current) setError(String(err));
    } finally {
      if (generation.current === current) setSaving(false);
    }
  }
  const disabled = saving || !preferences?.can_edit;
  const tooLong = new TextEncoder().encode(instructions).length > 8192;
  return (
    <SettingsPanel
      title={`Configure Tidebreak · ${channelId}`}
      description="Choose how Tidebreak works in this Slack channel. Model, harness, and instructions apply to new conversations. Existing work keeps its settings."
      busy={loading || saving}
    >
      {loading ? (
        <p role="status" className="text-sm text-muted-foreground">
          Loading channel settings…
        </p>
      ) : !preferences ? (
        <>
          <SettingsError>{error}</SettingsError>
          <Button variant="outline" onClick={() => setReload((n) => n + 1)}>
            Try again
          </Button>
        </>
      ) : (
        <>
          <p className="font-mono text-xs text-muted-foreground">
            {preferences.workspace_identity} · {preferences.channel_id}
          </p>
          {!preferences.can_edit && (
            <p className="text-sm text-muted-foreground">
              An administrator can change these shared channel settings.
            </p>
          )}
          <SettingsSection
            title="Agent"
            description="Unset harness choices inherit this Tidebreak instance’s default. Each harness supplies its default model. Gateway controls which models and tools the agent can use."
          >
            <SettingsField label="Harness">
              <Select
                value={preferences.harness ?? "inherit"}
                onValueChange={(value) =>
                  void save({
                    harness:
                      value === "inherit" ? null : (value as HarnessKind),
                    model: null,
                  })
                }
                disabled={disabled}
              >
                <SelectTrigger aria-label="Harness">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="inherit">Use instance default</SelectItem>
                  {preferences.harness &&
                    !harnesses.includes(preferences.harness) && (
                      <SelectItem value={preferences.harness}>
                        {LABELS[preferences.harness]} (unavailable)
                      </SelectItem>
                    )}
                  {harnesses.map((h) => (
                    <SelectItem key={h} value={h}>
                      {LABELS[h]}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </SettingsField>
            <SettingsField
              label="Model"
              hint={
                !preferences.harness
                  ? "Choose a harness to select a model for this channel."
                  : "The catalog lists models available to the selected harness. Execution still requires access through the Slack connection."
              }
            >
              <Select
                value={preferences.model ?? "inherit"}
                onValueChange={(value) =>
                  void save({ model: value === "inherit" ? null : value })
                }
                disabled={disabled || !preferences.harness}
              >
                <SelectTrigger aria-label="Model">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="inherit">
                    {preferences.harness === "internal"
                      ? "Use instance default"
                      : "Use harness default"}
                  </SelectItem>
                  {preferences.model &&
                    !models.some((m) => m.id === preferences.model) && (
                      <SelectItem value={preferences.model}>
                        {preferences.model} (unavailable)
                      </SelectItem>
                    )}
                  {models.map((m) => (
                    <SelectItem key={m.id} value={m.id}>
                      {m.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </SettingsField>
            {catalogError && <SettingsError>{catalogError}</SettingsError>}
          </SettingsSection>
          <SettingsSection title="Conversation behavior">
            <SettingsField
              label="Respond automatically"
              hint="Reply to messages in threads where Tidebreak is already participating. When off, mention Tidebreak to get a reply. This does not start conversations from every channel message or interrupt ongoing work."
            >
              <Switch
                aria-label="Respond automatically"
                checked={preferences.respond_automatically ?? true}
                disabled={disabled}
                onCheckedChange={(value) =>
                  void save({ respond_automatically: value })
                }
              />
            </SettingsField>
            <SettingsField
              label="Channel instructions"
              hint="Added to new conversations alongside instance instructions. Changes save when you leave this field. Maximum 8,192 bytes."
            >
              <Textarea
                aria-label="Channel instructions"
                className="min-h-40"
                value={instructions}
                disabled={disabled}
                onChange={(event) => setInstructions(event.target.value)}
                onBlur={() => {
                  if (!tooLong && instructions !== preferences.instructions)
                    void save({ instructions });
                }}
                placeholder="Keep replies brief and link to our runbooks."
                aria-invalid={tooLong}
              />
            </SettingsField>
            {tooLong && (
              <SettingsError>
                Shorten the instructions to 8,192 bytes before saving.
              </SettingsError>
            )}
          </SettingsSection>
          <SettingsSection title="Access">
            <p className="text-sm text-muted-foreground">
              This channel inherits the instance’s GitHub App repository access.
              Gateway manages credentials, tools, execution limits, and sandbox
              infrastructure.
            </p>
          </SettingsSection>
          {error && <SettingsError>{error}</SettingsError>}
          {saving && (
            <p className="text-sm text-muted-foreground" role="status">
              Saving…
            </p>
          )}
        </>
      )}
    </SettingsPanel>
  );
}
