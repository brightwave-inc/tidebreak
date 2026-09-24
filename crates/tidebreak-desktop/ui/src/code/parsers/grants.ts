import { isRecord, onlyKeys } from "../../lib/wireDecode";
import type { CodeGrantSnapshot, CodeConnectPage } from "../../api/types";
import {
  nonEmptyLine,
  optionalLine,
  optionalBlock,
  wireId,
  timestamp,
  optionalTimestamp,
} from "./shared";

export function parseCodeGrant(value: unknown): CodeGrantSnapshot | null {
  if (
    !isRecord(value) ||
    !onlyKeys<CodeGrantSnapshot>(value, [
      "id",
      "kind",
      "channel_kind",
      "external_identity",
      "display_name",
      "workspace_identity",
      "workspace_name",
      "avatar_url",
      "rotated_at",
      "created_at",
      "revoked_at",
      "revoked_reason",
    ]) ||
    !wireId(value.id) ||
    !optionalLine(value.kind) ||
    !nonEmptyLine(value.channel_kind) ||
    !nonEmptyLine(value.external_identity) ||
    !optionalLine(value.display_name) ||
    !nonEmptyLine(value.workspace_identity) ||
    !optionalLine(value.workspace_name) ||
    !optionalLine(value.avatar_url) ||
    !timestamp(value.created_at) ||
    !optionalTimestamp(value.rotated_at) ||
    !optionalTimestamp(value.revoked_at) ||
    !optionalBlock(value.revoked_reason)
  ) {
    return null;
  }
  return {
    id: value.id,
    ...(value.kind !== undefined ? { kind: value.kind } : {}),
    channel_kind: value.channel_kind,
    external_identity: value.external_identity,
    ...(value.display_name !== undefined
      ? { display_name: value.display_name }
      : {}),
    workspace_identity: value.workspace_identity,
    ...(value.workspace_name !== undefined
      ? { workspace_name: value.workspace_name }
      : {}),
    ...(value.avatar_url !== undefined ? { avatar_url: value.avatar_url } : {}),
    created_at: value.created_at,
    ...(value.rotated_at !== undefined ? { rotated_at: value.rotated_at } : {}),
    ...(value.revoked_at !== undefined ? { revoked_at: value.revoked_at } : {}),
    ...(value.revoked_reason !== undefined
      ? { revoked_reason: value.revoked_reason }
      : {}),
  };
}

export function parseCodeGrantList(value: unknown): CodeGrantSnapshot[] | null {
  if (!Array.isArray(value)) return null;
  const grants: CodeGrantSnapshot[] = [];
  for (const entry of value) {
    const grant = parseCodeGrant(entry);
    if (!grant) return null;
    grants.push(grant);
  }
  return grants;
}

export function parseCodeConnectPage(value: unknown): CodeConnectPage | null {
  if (
    !isRecord(value) ||
    !nonEmptyLine(value.channel_kind) ||
    !nonEmptyLine(value.display_name) ||
    !nonEmptyLine(value.workspace_name) ||
    !optionalLine(value.avatar_url) ||
    !nonEmptyLine(value.state) ||
    !wireId(value.csrf) ||
    !timestamp(value.expires_at) ||
    (value.inference_sponsorship_supported !== undefined &&
      typeof value.inference_sponsorship_supported !== "boolean")
  ) {
    return null;
  }
  return {
    inference_sponsorship_supported:
      value.inference_sponsorship_supported === true,
    channel_kind: value.channel_kind,
    display_name: value.display_name,
    workspace_name: value.workspace_name,
    ...(value.avatar_url !== undefined ? { avatar_url: value.avatar_url } : {}),
    state: value.state,
    csrf: value.csrf,
    expires_at: value.expires_at,
  };
}
