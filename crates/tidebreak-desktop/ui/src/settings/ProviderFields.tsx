import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { SettingsField } from "./primitives";

/**
 * Web search and code execution both hold one credential per provider in a
 * fixed keychain slot and run through one provider at a time. These are the two
 * controls that shape gives every such panel: a key field per slot, and the
 * choice of which provider is live.
 */

// Radix Select reserves the empty string, so "Disabled" (no provider) rides on
// a sentinel value the wire never carries.
const NO_PROVIDER = "__disabled__";

/** Longest key the local credential endpoints accept. */
const MAX_CREDENTIAL_LENGTH = 8_192;

export type ProviderOption<Kind extends string> = {
  kind: Kind;
  label: string;
};

/**
 * The host-enforced bound on one provider request. Both surfaces persist a
 * millisecond policy, and both read and write it in seconds — the unit the
 * bounds are actually reasoned about in, and the only one worth typing.
 */
export function TimeoutSecondsField({
  label,
  minSeconds,
  maxSeconds,
  value,
  disabled,
  onChange,
}: {
  label: string;
  minSeconds: number;
  maxSeconds: number;
  value: string;
  disabled?: boolean;
  onChange: (value: string) => void;
}) {
  return (
    <SettingsField
      label={`${label} (seconds)`}
      hint={`Between ${minSeconds} and ${maxSeconds} seconds.`}
    >
      <Input
        type="number"
        inputMode="numeric"
        min={minSeconds}
        max={maxSeconds}
        step="1"
        value={value}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
      />
    </SettingsField>
  );
}

/**
 * Convert what the user typed into the millisecond policy the wire carries, or
 * report why it cannot be sent. Bounds are checked here so an out-of-range
 * timeout never reaches the server, which enforces the same range itself.
 */
export function timeoutMsFromSeconds(
  value: string,
  minSeconds: number,
  maxSeconds: number,
): { timeoutMs: number } | { error: string } {
  const seconds = Number(value);
  if (
    value.trim() === "" ||
    !Number.isFinite(seconds) ||
    seconds < minSeconds ||
    seconds > maxSeconds
  ) {
    return {
      error: `Timeout must be between ${minSeconds} and ${maxSeconds} seconds.`,
    };
  }
  return { timeoutMs: Math.round(seconds * 1000) };
}

/**
 * The single provider a host-owned tool runs through, alongside an explicit
 * Disabled choice — the safe state, and the only way to take the tool out of
 * service without discarding what is configured.
 */
export function ActiveProviderField<Kind extends string>({
  options,
  value,
  disabled,
  onChange,
}: {
  options: ProviderOption<Kind>[];
  value: Kind | "";
  disabled?: boolean;
  onChange: (value: Kind | "") => void;
}) {
  return (
    <SettingsField label="Provider">
      <Select
        value={value === "" ? NO_PROVIDER : value}
        disabled={disabled}
        onValueChange={(next) =>
          onChange(next === NO_PROVIDER ? "" : (next as Kind))
        }
      >
        <SelectTrigger aria-label="Provider">
          <SelectValue />
        </SelectTrigger>
        <SelectContent scrollButtons={false}>
          <SelectItem value={NO_PROVIDER}>Disabled</SelectItem>
          {options.map((option) => (
            <SelectItem key={option.kind} value={option.kind}>
              {option.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </SettingsField>
  );
}

/**
 * A write-only key field for one provider's credential slot, with the remove
 * action beside it. A saved key is never read back, so the field reports only
 * that one is there and stays empty until the user types a replacement.
 */
export function ProviderCredentialField({
  provider,
  hasCredential,
  value,
  disabled,
  removing,
  onChange,
  onRemove,
}: {
  provider: string;
  hasCredential: boolean;
  value: string;
  disabled?: boolean;
  removing?: boolean;
  onChange: (value: string) => void;
  onRemove: () => void;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      <SettingsField
        label={`${provider} API key`}
        hint={
          hasCredential
            ? "A key is already saved. Type a new one to replace it."
            : undefined
        }
      >
        <Input
          type="password"
          placeholder={
            hasCredential
              ? "Saved — leave blank to keep it"
              : `Paste your ${provider} API key`
          }
          value={value}
          maxLength={MAX_CREDENTIAL_LENGTH}
          autoComplete="new-password"
          disabled={disabled}
          onChange={(event) => onChange(event.target.value)}
        />
      </SettingsField>
      {hasCredential && (
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="self-start"
          disabled={disabled}
          onClick={onRemove}
        >
          {removing ? "Removing…" : `Remove saved ${provider} key`}
        </Button>
      )}
    </div>
  );
}
