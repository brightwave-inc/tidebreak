import { useEffect, useState } from "react";

import type { ApiClient, CitationFormat } from "../api";
import { CITATION_FORMAT_OPTIONS } from "../CitationFormats";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { SettingsError, SettingsField, SettingsPanel, SettingsSection } from "./primitives";

/**
 * The citation format conversations follow unless they set their own.
 *
 * Only the default lives here. The per-chat picker in the message bar is the
 * one a reader reaches for mid-conversation, and a chat that overrode the
 * default keeps its choice when this changes.
 */
export function CitationsPanel({
  client,
  onChanged,
}: {
  client: ApiClient;
  onChanged?: () => void;
}) {
  const [format, setFormat] = useState<CitationFormat | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setError(null);
    void (async () => {
      try {
        const settings = await client.getSettings();
        if (!cancelled) setFormat(settings.citation_format);
      } catch (err) {
        if (!cancelled) setError(String(err));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [client]);

  async function save(next: CitationFormat) {
    const previous = format;
    setFormat(next);
    setSaving(true);
    setError(null);
    try {
      const settings = await client.putSettings({ citation_format: next });
      setFormat(settings.citation_format);
      onChanged?.();
    } catch (err) {
      setFormat(previous);
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }

  const selected = CITATION_FORMAT_OPTIONS.find((option) => option.value === format);

  return (
    <SettingsPanel
      title="Citations"
      description="How answers cite the sources they were built from."
      busy={format === null}
    >
      <SettingsSection>
        <SettingsField label="Default format" hint={selected?.description}>
          <Select
            value={format ?? undefined}
            disabled={format === null || saving}
            onValueChange={(next) => void save(next as CitationFormat)}
          >
            <SelectTrigger aria-label="Default citation format">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {CITATION_FORMAT_OPTIONS.map((option) => (
                <SelectItem key={option.value} value={option.value}>
                  {option.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </SettingsField>
        {error && <SettingsError>{error}</SettingsError>}
      </SettingsSection>
    </SettingsPanel>
  );
}
