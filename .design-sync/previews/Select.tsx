import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
} from "tidebreak-desktop-ui";

export function ModelMenuOpen() {
  return (
    <div style={{ maxWidth: 280 }}>
      <Select defaultValue="claude-sonnet-4-5" defaultOpen>
        <SelectTrigger aria-label="Model">
          <SelectValue />
        </SelectTrigger>
        <SelectContent scrollButtons={false}>
          <SelectGroup>
            <SelectLabel>Anthropic</SelectLabel>
            <SelectItem value="claude-sonnet-4-5">Claude Sonnet 4.5</SelectItem>
            <SelectItem value="claude-opus-4-1">Claude Opus 4.1</SelectItem>
          </SelectGroup>
          <SelectSeparator />
          <SelectGroup>
            <SelectLabel>OpenAI</SelectLabel>
            <SelectItem value="gpt-5-codex">GPT-5 Codex</SelectItem>
            <SelectItem value="gpt-5-mini" disabled>
              GPT-5 mini (no API key)
            </SelectItem>
          </SelectGroup>
        </SelectContent>
      </Select>
    </div>
  );
}

export function TriggerStates() {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12, maxWidth: 280 }}>
      <Select defaultValue="claude-code">
        <SelectTrigger aria-label="Harness">
          <SelectValue />
        </SelectTrigger>
        <SelectContent scrollButtons={false}>
          <SelectItem value="claude-code">Claude Code</SelectItem>
          <SelectItem value="codex">Codex</SelectItem>
        </SelectContent>
      </Select>
      <Select>
        <SelectTrigger aria-label="Repo">
          <SelectValue placeholder="No repos" />
        </SelectTrigger>
        <SelectContent scrollButtons={false}>
          <SelectItem value="tidebreak">tidebreak</SelectItem>
        </SelectContent>
      </Select>
      <Select defaultValue="ask">
        <SelectTrigger size="sm" aria-label="Permission mode">
          <SelectValue />
        </SelectTrigger>
        <SelectContent scrollButtons={false}>
          <SelectItem value="ask">Ask before edits</SelectItem>
        </SelectContent>
      </Select>
      <Select defaultValue="main" disabled>
        <SelectTrigger aria-label="Base ref">
          <SelectValue />
        </SelectTrigger>
        <SelectContent scrollButtons={false}>
          <SelectItem value="main">main</SelectItem>
        </SelectContent>
      </Select>
    </div>
  );
}
