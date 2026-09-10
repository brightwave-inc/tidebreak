import type { HarnessKind } from "../api/types";
export type ChannelPreferences = {
  harness: HarnessKind | null;
  model: string | null;
  respond_automatically: boolean | null;
  instructions: string;
};
export type ChannelPreferencesSnapshot = ChannelPreferences & {
  channel_id: string;
  workspace_identity: string;
  settings_path: string;
  can_edit: boolean;
};
export function parseChannelPreferences(
  value: unknown,
): ChannelPreferencesSnapshot | null {
  if (!value || typeof value !== "object") return null;
  const v = value as Record<string, unknown>;
  if (
    v.harness !== null &&
    !["internal", "claude_code", "codex", "opencode", "grok"].includes(
      String(v.harness),
    )
  )
    return null;
  if (v.model !== null && typeof v.model !== "string") return null;
  if (
    v.respond_automatically !== null &&
    typeof v.respond_automatically !== "boolean"
  )
    return null;
  if (
    typeof v.instructions !== "string" ||
    typeof v.channel_id !== "string" ||
    typeof v.workspace_identity !== "string" ||
    typeof v.settings_path !== "string" ||
    typeof v.can_edit !== "boolean"
  )
    return null;
  return v as ChannelPreferencesSnapshot;
}
