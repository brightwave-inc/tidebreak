import { useCallback, useEffect, useState } from "react";

import type { ApiClient } from "@/api/client";
import { friendlyErrorMessage } from "@/lib/utils";
import { InstructionsField } from "./InstructionsField";
import { SettingsError, SettingsPanel, SettingsSection } from "./primitives";

type InstructionsClient = Pick<
  ApiClient,
  "getPersonalInstructions" | "putPersonalInstructions"
>;

/**
 * Your standing instructions for Tidebreak: the preferences it follows in
 * every conversation, before any project adds its own.
 */
export function InstructionsPanel({ client }: { client: InstructionsClient }) {
  const [saved, setSaved] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    void client
      .getPersonalInstructions()
      .then((next) => {
        if (!cancelled) setSaved(next.instructions);
      })
      .catch((caught: unknown) => {
        if (!cancelled) {
          setError(
            friendlyErrorMessage(caught, "Could not load your instructions."),
          );
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  useEffect(() => load(), [load]);

  return (
    <SettingsPanel
      title="Instructions"
      description="Tell Tidebreak how you want it to work. It follows these in every conversation, and each project can add its own on the project’s page."
      busy={loading}
    >
      {saved === null ? (
        loading ? (
          <p role="status" className="text-sm text-muted-foreground">
            Loading your instructions…
          </p>
        ) : (
          <SettingsError
            title="Could not load your instructions"
            onRetry={load}
          >
            {error}
          </SettingsError>
        )
      ) : (
        <SettingsSection title="Personal instructions">
          <InstructionsField
            label="Personal instructions"
            hint="Applies to every conversation, starting with your next message. Changes save when you leave the field. Other coding engines, such as Claude Code and Codex, read their own instruction files."
            placeholder="Answer in British English. Lead with the answer, then the detail."
            saved={saved}
            onSave={async (instructions) => {
              const next = await client.putPersonalInstructions(instructions);
              setSaved(next.instructions);
            }}
          />
        </SettingsSection>
      )}
    </SettingsPanel>
  );
}
