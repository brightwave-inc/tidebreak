import { Switch } from "tidebreak-desktop-ui";

export function States() {
  return (
    <div style={{ display: "flex", gap: 16, alignItems: "center" }}>
      <Switch checked aria-label="On" />
      <Switch aria-label="Off" />
      <Switch disabled checked aria-label="On, disabled" />
      <Switch disabled aria-label="Off, disabled" />
    </div>
  );
}

export function SettingsRows() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 14, maxWidth: 420 }}>
      <label className="flex items-center justify-between gap-4">
        <span className="flex flex-col">
          <span className="text-sm font-medium">Web search</span>
          <span className="text-xs text-muted-foreground">
            Let assistants search the web during a turn.
          </span>
        </span>
        <Switch checked />
      </label>
      <label className="flex items-center justify-between gap-4">
        <span className="flex flex-col">
          <span className="text-sm font-medium">Voice transcription</span>
          <span className="text-xs text-muted-foreground">
            Transcribe microphone input into the composer.
          </span>
        </span>
        <Switch />
      </label>
      <label className="flex items-center justify-between gap-4">
        <span className="flex flex-col">
          <span className="text-sm font-medium">Windows updater feed</span>
          <span className="text-xs text-muted-foreground">
            Managed by your release channel.
          </span>
        </span>
        <Switch disabled checked />
      </label>
    </div>
  );
}
